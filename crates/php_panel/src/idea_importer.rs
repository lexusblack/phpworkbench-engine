use std::path::Path;
use serde_json::{json, Value};
use regex::Regex;
use crate::{
    HttpAuthSettings, HttpBodyKind, HttpHistoryEntry, HttpHistoryFile, KeyValueRow,
    RawContentType,
};

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

    let mut local_info = std::collections::HashMap::new();
    for cap in local_ds_re.captures_iter(&local_content) {
        let uuid = cap[1].to_string();
        let body = &cap[2];
        let user = user_re.captures(body).map(|c| c[1].to_string()).unwrap_or_default();
        let ssh = ssh_re.is_match(body);
        local_info.insert(uuid, (user, ssh));
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

        let (user, ssh) = local_info.get(&uuid).cloned().unwrap_or_default();
        let (driver, host, port, database) = parse_jdbc_url(&jdbc_url);

        let mut conn = json!({
            "name": name,
            "driver": driver,
            "host": host,
            "port": port,
            "database": database,
            "user": user,
            "password": "",
        });
        if ssh {
            conn["ssh_tunnel"] = json!({ "enabled": true });
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

/// Attempts to parse IntelliJ / PhpStorm `.idea/webServers.xml` and `.idea/deployment.xml`
/// into the JSON format expected by PhpWorkbench's `deploy.json`.
pub fn import_idea_servers(root: &Path) -> Option<Value> {
    let idea_dir = root.join(".idea");
    let web_servers_path = idea_dir.join("webServers.xml");
    if !web_servers_path.is_file() {
        return None;
    }
    let xml = std::fs::read_to_string(&web_servers_path).ok()?;

    // Read default server name from deployment.xml
    let dep_xml_path = idea_dir.join("deployment.xml");
    let dep_xml = std::fs::read_to_string(&dep_xml_path).unwrap_or_default();
    let default_server_re = Regex::new(r#"serverName="([^"]+)""#).ok()?;
    let default_server = default_server_re.captures(&dep_xml).map(|c| c[1].to_string());

    let ws_re = Regex::new(r#"(?s)<webServer\s+([^>]*?)>(.*?)</webServer>"#).ok()?;
    let name_re = Regex::new(r#"name="([^"]+)""#).ok()?;
    let url_re = Regex::new(r#"url="([^"]+)""#).ok()?;
    let ft_re = Regex::new(r#"<fileTransfer\s+([^>]+)>"#).ok()?;
    let attr_re = Regex::new(r#"(\w+)="([^"]*)""#).ok()?;

    let mut servers = Vec::new();

    for cap in ws_re.captures_iter(&xml) {
        let ws_attrs = &cap[1];
        let body = &cap[2];

        let name = match name_re.captures(ws_attrs) {
            Some(c) => c[1].to_string(),
            None => continue,
        };
        let web_url = url_re.captures(ws_attrs).map(|c| c[1].to_string()).unwrap_or_default();

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

        servers.push(json!({
            "name": name,
            "protocol": protocol,
            "host": host,
            "port": port,
            "user": "",
            "remote_root": remote_root,
            "local_root": ".",
            "web_url": web_url,
            "upload_on_save": false,
            "exclude": ["vendor/", "node_modules/", ".git/"],
        }));
    }

    if servers.is_empty() {
        None
    } else {
        let mut res = json!({ "servers": servers });
        if let Some(def) = default_server {
            res["default"] = json!(def);
            res["active"] = json!(def);
        }
        Some(res)
    }
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
}
