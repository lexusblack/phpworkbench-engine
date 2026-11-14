use std::path::{Path, PathBuf};
use serde_json::{json, Value};
use regex::Regex;
use crate::{
    HttpAuthSettings, HttpBodyKind, HttpHistoryEntry, HttpHistoryFile, KeyValueRow,
    RawContentType,
};

/// Searches known JetBrains global configuration directories for a given options XML file
/// (e.g. `webServers.xml` or `sshConfigs.xml`). Returns candidates sorted by priority
/// (PhpStorm first, then newest version first).
pub fn find_jetbrains_global_files(filename: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    // macOS: ~/Library/Application Support/JetBrains/*/options/<filename>
    if let Ok(home) = std::env::var("HOME") {
        let mac_base = PathBuf::from(&home).join("Library/Application Support/JetBrains");
        if mac_base.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&mac_base) {
                for entry in entries.flatten() {
                    let file = entry.path().join("options").join(filename);
                    if file.is_file() {
                        candidates.push(file);
                    }
                }
            }
        }
        // Linux: ~/.config/JetBrains/*/options/<filename>
        let linux_base = PathBuf::from(&home).join(".config/JetBrains");
        if linux_base.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&linux_base) {
                for entry in entries.flatten() {
                    let file = entry.path().join("options").join(filename);
                    if file.is_file() {
                        candidates.push(file);
                    }
                }
            }
        }
    }

    // Windows: %APPDATA%\JetBrains\*\options\<filename>
    if let Ok(appdata) = std::env::var("APPDATA") {
        let win_base = PathBuf::from(&appdata).join("JetBrains");
        if win_base.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&win_base) {
                for entry in entries.flatten() {
                    let file = entry.path().join("options").join(filename);
                    if file.is_file() {
                        candidates.push(file);
                    }
                }
            }
        }
    }

    candidates.sort_by(|a, b| {
        let a_str = a.to_string_lossy();
        let b_str = b.to_string_lossy();
        let a_php = a_str.contains("PhpStorm");
        let b_php = b_str.contains("PhpStorm");
        match (a_php, b_php) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b_str.cmp(&a_str),
        }
    });

    candidates
}

/// On macOS, inspects the system Keychain metadata for generic passwords under `service`
/// and extracts the unencrypted `"acct"` (username) attribute without triggering a user prompt.
pub fn get_keychain_account(service: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("security")
            .args(&["find-generic-password", "-s", service])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains("\"acct\"<blob>=") {
                if let Some(eq_pos) = line.find("=\"") {
                    let rest = &line[eq_pos + 2..];
                    if let Some(end) = rest.find('"') {
                        let acct = &rest[..end];
                        if !acct.is_empty() && acct != "<NULL>" {
                            return Some(acct.to_string());
                        }
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = service;
    }
    None
}

/// Attempts to parse IntelliJ / PhpStorm `.idea/dataSources.xml` (and `.idea/dataSources.local.xml`)
/// into the JSON format expected by PhpWorkbench's `databases.json`.
pub fn import_idea_databases(root: &Path) -> Option<Value> {
    let idea_dir = root.join(".idea");
    let xml_path = idea_dir.join("dataSources.xml");
    if !xml_path.is_file() {
        return None;
    }
    let xml = std::fs::read_to_string(&xml_path).ok()?;

    // Read local file for usernames and ssh settings if present
    let local_xml = idea_dir.join("dataSources.local.xml");
    let local_content = std::fs::read_to_string(&local_xml).unwrap_or_default();

    let ds_re = Regex::new(r#"(?s)<data-source\s+[^>]*?name="([^"]+)"[^>]*?uuid="([^"]+)"[^>]*?>(.*?)</data-source>"#).ok()?;
    let url_re = Regex::new(r#"<jdbc-url>([^<]+)</jdbc-url>"#).ok()?;

    let local_ds_re = Regex::new(r#"(?s)<data-source\s+[^>]*?uuid="([^"]+)"[^>]*?>(.*?)</data-source>"#).ok()?;
    let user_re = Regex::new(r#"<user-name>([^<]+)</user-name>"#).ok()?;
    let ssh_re = Regex::new(r#"(?s)<ssh-properties>.*?<enabled>true</enabled>.*?</ssh-properties>"#).ok()?;
    let ssh_id_re = Regex::new(r#"<ssh-config-id>([^<]+)</ssh-config-id>"#).ok()?;

    // Parse global sshConfigs.xml if available to resolve SSH tunnel host/port/username
    let mut ssh_configs: std::collections::HashMap<String, (String, u16, String)> = std::collections::HashMap::new();
    let ssh_cfg_re = Regex::new(r#"<sshConfig\s+([^>]+)>"#).ok();
    let attr_re = Regex::new(r#"(\w+)="([^"]*)""#).ok();
    if let (Some(cfg_re), Some(a_re)) = (ssh_cfg_re.as_ref(), attr_re.as_ref()) {
        for global_ssh in find_jetbrains_global_files("sshConfigs.xml") {
            if let Ok(ssh_content) = std::fs::read_to_string(&global_ssh) {
                for cap in cfg_re.captures_iter(&ssh_content) {
                    let attrs_str = &cap[1];
                    let mut id = String::new();
                    let mut host = String::new();
                    let mut port = 22u16;
                    let mut user = String::new();
                    for attr in a_re.captures_iter(attrs_str) {
                        match &attr[1] {
                            "id" => id = attr[2].to_string(),
                            "host" => host = attr[2].to_string(),
                            "port" => {
                                if let Ok(p) = attr[2].parse::<u16>() {
                                    port = p;
                                }
                            }
                            "username" => user = attr[2].to_string(),
                            _ => {}
                        }
                    }
                    if !id.is_empty() && !host.is_empty() {
                        ssh_configs.entry(id).or_insert((host, port, user));
                    }
                }
            }
        }
    }

    let mut local_info = std::collections::HashMap::new();
    for cap in local_ds_re.captures_iter(&local_content) {
        let uuid = cap[1].to_string();
        let body = &cap[2];
        let user = user_re.captures(body).map(|c| c[1].to_string()).unwrap_or_default();
        let ssh = ssh_re.is_match(body);
        let ssh_id = ssh_id_re.captures(body).map(|c| c[1].to_string());
        local_info.insert(uuid, (user, ssh, ssh_id));
    }

    let mut connections = Vec::new();

    for cap in ds_re.captures_iter(&xml) {
        let name = cap[1].to_string();
        let uuid = cap[2].to_string();
        let body = &cap[3];

        let jdbc_url = match url_re.captures(body) {
            Some(u) => u[1].to_string(),
            None => continue,
        };

        let (user, ssh, ssh_id) = local_info.get(&uuid).cloned().unwrap_or_default();
        let (driver, host, port, database) = parse_jdbc_url(&jdbc_url);

        let keychain_service = format!("IntelliJ Platform DB \u{2014} {uuid}");
        let mut resolved_user = user;
        if resolved_user.is_empty() {
            if let Some(acct) = get_keychain_account(&keychain_service) {
                resolved_user = acct;
            }
        }

        let mut conn = json!({
            "name": name,
            "driver": driver,
            "host": host,
            "port": port,
            "database": database,
            "user": resolved_user,
            "password": "",
            "keychain_service": keychain_service,
            "keychain": true,
        });
        if ssh {
            if let Some(id) = ssh_id.as_ref().and_then(|id| ssh_configs.get(id)) {
                conn["ssh_tunnel"] = json!({
                    "enabled": true,
                    "host": id.0,
                    "port": id.1,
                    "user": id.2,
                });
            } else {
                conn["ssh_tunnel"] = json!({ "enabled": true });
            }
        }
        connections.push(conn);
    }

    if connections.is_empty() {
        None
    } else {
        Some(json!({ "connections": connections }))
    }
}

fn parse_jdbc_url(url: &str) -> (String, String, u16, String) {
    if let Some(rest) = url.strip_prefix("jdbc:") {
        if let Some(sqlite_path) = rest.strip_prefix("sqlite:") {
            return ("sqlite".to_string(), "".to_string(), 0, sqlite_path.to_string());
        }

        let parts: Vec<&str> = rest.splitn(2, "://").collect();
        if parts.len() == 2 {
            let proto = match parts[0] {
                "mariadb" => "mariadb",
                "postgresql" | "postgres" => "postgres",
                _ => "mysql",
            };

            let path_parts: Vec<&str> = parts[1].splitn(2, '/').collect();
            let host_port: Vec<&str> = path_parts[0].splitn(2, ':').collect();
            let host = host_port[0].to_string();
            let port: u16 = if host_port.len() == 2 {
                host_port[1].parse().unwrap_or(3306)
            } else if proto == "postgres" {
                5432
            } else {
                3306
            };
            let database = if path_parts.len() == 2 {
                path_parts[1].split('?').next().unwrap_or("").to_string()
            } else {
                "".to_string()
            };
            return (proto.to_string(), host, port, database);
        }
    }
    ("mysql".to_string(), "localhost".to_string(), 3306, "".to_string())
}

/// Attempts to parse IntelliJ / PhpStorm `.idea/webServers.xml` (or global PhpStorm `options/webServers.xml`)
/// and `.idea/deployment.xml` into the JSON format expected by PhpWorkbench's `deploy.json`.
pub fn import_idea_servers(root: &Path) -> Option<Value> {
    let idea_dir = root.join(".idea");
    if !idea_dir.is_dir() {
        return None;
    }
    if !idea_dir.join("deployment.xml").is_file() && !idea_dir.join("webServers.xml").is_file() {
        return None;
    }

    // 1. Read deployment.xml for default server and project-mapped servers
    let dep_xml_path = idea_dir.join("deployment.xml");
    let dep_xml = std::fs::read_to_string(&dep_xml_path).unwrap_or_default();
    let default_server_re = Regex::new(r#"serverName="([^"]+)""#).ok()?;
    let default_server = default_server_re.captures(&dep_xml).map(|c| c[1].to_string());

    let paths_re = Regex::new(r#"<paths\s+name="([^"]+)""#).ok()?;
    let mut project_servers = Vec::new();
    for cap in paths_re.captures_iter(&dep_xml) {
        let name = cap[1].to_string();
        if !project_servers.contains(&name) {
            project_servers.push(name);
        }
    }

    // 2. Locate webServers.xml: check project .idea first, then global JetBrains options
    let mut ws_paths = Vec::new();
    let local_ws = idea_dir.join("webServers.xml");
    if local_ws.is_file() {
        ws_paths.push(local_ws);
    }
    for global_ws in find_jetbrains_global_files("webServers.xml") {
        ws_paths.push(global_ws);
    }

    if ws_paths.is_empty() {
        return None;
    }

    let ws_re = Regex::new(r#"(?s)<webServer\s+([^>]*?)>(.*?)</webServer>"#).ok()?;
    let name_re = Regex::new(r#"name="([^"]+)""#).ok()?;
    let url_re = Regex::new(r#"url="([^"]+)""#).ok()?;
    let ft_re = Regex::new(r#"<fileTransfer\s+([^>]+)>"#).ok()?;
    let attr_re = Regex::new(r#"(\w+)="([^"]*)""#).ok()?;

    let mut all_servers = std::collections::HashMap::new();

    for ws_path in ws_paths {
        let Ok(xml) = std::fs::read_to_string(&ws_path) else {
            continue;
        };

        for cap in ws_re.captures_iter(&xml) {
            let ws_attrs = &cap[1];
            let body = &cap[2];

            let name = match name_re.captures(ws_attrs) {
                Some(c) => c[1].to_string(),
                None => continue,
            };
            if all_servers.contains_key(&name) {
                continue;
            }

            let id_re = Regex::new(r#"id="([^"]+)""#).ok();
            let server_id = id_re.as_ref().and_then(|r| r.captures(ws_attrs)).map(|c| c[1].to_string());

            let mut user = String::new();
            let mut keychain_service = None;
            if let Some(ref id) = server_id {
                let service = format!("IntelliJ Platform Deployment \u{2014} {id}");
                if let Some(acct) = get_keychain_account(&service) {
                    user = acct;
                }
                keychain_service = Some(service);
            }

            let web_url = url_re
                .captures(ws_attrs)
                .map(|c| c[1].to_string())
                .unwrap_or_default();

            let mut host = String::new();
            let mut port: u16 = 22;
            let mut remote_root = String::new();
            let mut protocol = "sftp".to_string();

            if let Some(ft_cap) = ft_re.captures(body) {
                let attrs_str = &ft_cap[1];
                for attr in attr_re.captures_iter(attrs_str) {
                    match &attr[1] {
                        "host" => host = attr[2].to_string(),
                        "port" => {
                            if let Ok(p) = attr[2].parse::<u16>() {
                                port = p;
                                if p == 21 {
                                    protocol = "ftp".to_string();
                                }
                            }
                        }
                        "rootFolder" => remote_root = attr[2].to_string(),
                        _ => {}
                    }
                }
            }

            // Check if FTPS (dataProtectionLevel or shareSSLContext)
            if body.contains("dataProtectionLevel") || body.contains("shareSSLContext") {
                if protocol == "ftp" {
                    protocol = "ftps".to_string();
                }
            }

            let mut s_val = json!({
                "name": name,
                "protocol": protocol,
                "host": host,
                "port": port,
                "user": user,
                "password": "",
                "remote_root": remote_root,
                "local_root": ".",
                "web_url": web_url,
                "upload_on_save": false,
                "exclude": ["vendor/", "node_modules/", ".git/"],
                "keychain": true,
            });
            if let Some(svc) = keychain_service {
                s_val["keychain_service"] = json!(svc);
            }

            all_servers.insert(name.clone(), s_val);
        }

        // If we found the default server or mapped servers in this webServers.xml, no need to check older ones
        if let Some(ref def) = default_server {
            if all_servers.contains_key(def) {
                break;
            }
        } else if !all_servers.is_empty() {
            break;
        }
    }

    if all_servers.is_empty() {
        return None;
    }

    // Filter and order servers:
    let mut servers = Vec::new();
    if !project_servers.is_empty() {
        // If deployment.xml mapped servers for this project:
        // Put default server first if present
        if let Some(ref def) = default_server {
            if let Some(s) = all_servers.get(def) {
                servers.push(s.clone());
            }
        }
        for name in &project_servers {
            if Some(name) != default_server.as_ref() {
                if let Some(s) = all_servers.get(name) {
                    servers.push(s.clone());
                }
            }
        }
    } else {
        // If no project mappings, include all parsed servers, with default_server first
        if let Some(ref def) = default_server {
            if let Some(s) = all_servers.get(def) {
                servers.push(s.clone());
            }
        }
        for (name, s) in &all_servers {
            if Some(name) != default_server.as_ref() {
                servers.push(s.clone());
            }
        }
    }

    if servers.is_empty() {
        return None;
    }

    let active_name = default_server
        .clone()
        .or_else(|| servers.first().and_then(|s| s["name"].as_str().map(str::to_string)));

    let mut res = json!({ "servers": servers });
    if let Some(def) = active_name {
        res["default"] = json!(def);
        res["active"] = json!(def);
    }
    Some(res)
}

/// Attempts to parse HTTP requests from `.idea/httpRequests/`
pub fn import_idea_http_history(root: &Path) -> Option<HttpHistoryFile> {
    let http_requests_dir = root.join(".idea/httpRequests");
    if !http_requests_dir.is_dir() {
        return None;
    }

    let mut entries = Vec::new();
    let log_file = http_requests_dir.join("http-requests-log.http");
    let files_to_read = if log_file.is_file() {
        vec![log_file]
    } else {
        let mut list = Vec::new();
        if let Ok(dir_entries) = std::fs::read_dir(&http_requests_dir) {
            for entry in dir_entries.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|ext| ext == "http" || ext == "rest") {
                    list.push(p);
                }
            }
        }
        list
    };

    for file_path in files_to_read {
        if let Ok(content) = std::fs::read_to_string(file_path) {
            parse_http_file_entries(&content, &mut entries);
        }
    }

    if entries.is_empty() {
        None
    } else {
        Some(HttpHistoryFile { entries })
    }
}

fn parse_http_file_entries(content: &str, out: &mut Vec<HttpHistoryEntry>) {
    let method_re = match Regex::new(r#"^(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\s+(\S+)"#) {
        Ok(r) => r,
        Err(_) => return,
    };

    for block in content.split("###") {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut lines = trimmed.lines().map(str::trim);
        let mut method = String::new();
        let mut url = String::new();
        let mut headers = Vec::new();
        let mut body_lines = Vec::new();
        let mut in_body = false;

        for line in lines.by_ref() {
            if in_body {
                body_lines.push(line);
            } else if line.is_empty() {
                in_body = true;
            } else if method.is_empty() {
                if let Some(caps) = method_re.captures(line) {
                    method = caps[1].to_string();
                    url = caps[2].to_string();
                }
            } else if let Some((k, v)) = line.split_once(':') {
                headers.push(KeyValueRow {
                    enabled: true,
                    key: k.trim().to_string(),
                    value: v.trim().to_string(),
                });
            }
        }

        if !method.is_empty() && !url.is_empty() {
            let raw_body = body_lines.join("\n");
            let is_json = raw_body.trim_start().starts_with('{') || raw_body.trim_start().starts_with('[');

            out.push(HttpHistoryEntry {
                id: format!("idea-import-{}", out.len() + 1),
                timestamp: String::new(),
                date: String::new(),
                method,
                url,
                subtab: None,
                params: Vec::new(),
                auth: HttpAuthSettings::default(),
                headers,
                body_kind: if raw_body.is_empty() { HttpBodyKind::Raw } else { HttpBodyKind::Raw },
                raw_content_type: if is_json { RawContentType::Json } else { RawContentType::Text },
                raw_body,
                body_fields: Vec::new(),
                follow_redirects: true,
                timeout_secs: 30,
                verify_tls: true,
                status: 200,
                time_ms: 0,
                request_headers: Vec::new(),
                response_headers: Vec::new(),
                response_body: String::new(),
                error: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jdbc_url() {
        let (p, h, port, db) = parse_jdbc_url("jdbc:mysql://localhost:3306/cross-rental");
        assert_eq!(p, "mysql");
        assert_eq!(h, "localhost");
        assert_eq!(port, 3306);
        assert_eq!(db, "cross-rental");

        let (p, h, port, db) = parse_jdbc_url("jdbc:postgresql://db.host:5432/main_db?ssl=true");
        assert_eq!(p, "postgres");
        assert_eq!(h, "db.host");
        assert_eq!(port, 5432);
        assert_eq!(db, "main_db");

        let (p, _h, _port, path) = parse_jdbc_url("jdbc:sqlite:/tmp/test.db");
        assert_eq!(p, "sqlite");
        assert_eq!(path, "/tmp/test.db");
    }

    #[test]
    fn test_import_newsystems_project() {
        let p = Path::new("/Users/admin/PhpstormProjects/newsystems");
        if p.is_dir() {
            let res = import_idea_servers(p).expect("import servers");
            assert_eq!(res["default"], json!("newdemo"));
            assert_eq!(res["active"], json!("newdemo"));
            let servers = res["servers"].as_array().expect("servers array");
            assert!(!servers.is_empty());
            assert_eq!(servers[0]["name"], json!("newdemo"));
            assert_eq!(servers[0]["protocol"], json!("ftps"));
            assert_eq!(servers[0]["user"], json!("olek_all"));
            assert_eq!(servers[0]["keychain"], json!(true));
            assert!(servers[0]["keychain_service"].as_str().unwrap().starts_with("IntelliJ Platform Deployment"));

            let db_res = import_idea_databases(p).expect("import databases");
            let conns = db_res["connections"].as_array().expect("connections");
            assert_eq!(conns[0]["name"], json!("newsystem@localhost"));
            assert_eq!(conns[0]["user"], json!("newsystem"));
            assert_eq!(conns[0]["keychain"], json!(true));
            assert!(conns[0]["keychain_service"].as_str().unwrap().starts_with("IntelliJ Platform DB"));
        }
    }
}
