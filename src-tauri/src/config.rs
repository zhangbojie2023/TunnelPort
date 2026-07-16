use serde::{Deserialize, Serialize};

/// A single tunnel profile (connection configuration for one target machine)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TunnelProfile {
    pub id: String,
    pub name: String,
    pub target_host: String,
    pub target_user: String,
    #[serde(default)]
    pub password: Option<String>,
    pub ssh_port: u16,
    pub socks_port: u16,
    pub http_proxy_port: u16,
    #[serde(default)]
    pub ssh_key_path: Option<String>,
    #[serde(default = "default_true")]
    pub auto_proxy: bool,
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    #[serde(default)]
    pub test_url: Option<String>,
}

fn default_true() -> bool {
    true
}

impl TunnelProfile {
    pub fn new(name: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            target_host: String::new(),
            target_user: String::new(),
            password: None,
            ssh_port: 22,
            socks_port: 1080,
            http_proxy_port: 1081,
            ssh_key_path: None,
            auto_proxy: true,
            auto_reconnect: true,
            test_url: None,
        }
    }

    pub fn ssh_args(&self) -> Vec<String> {
        let mut args = vec![
            "-D".to_string(),
            self.socks_port.to_string(),
            "-N".to_string(),
            "-C".to_string(),
            "-o".to_string(),
            "ServerAliveInterval=60".to_string(),
            "-o".to_string(),
            "ServerAliveCountMax=3".to_string(),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
        ];

        if let Some(key_path) = &self.ssh_key_path {
            args.push("-i".to_string());
            args.push(key_path.clone());
        }

        args.push(format!("{}@{}", self.target_user, self.target_host));
        args
    }
}

/// Application-level config (profiles + global settings)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppConfig {
    pub profiles: Vec<TunnelProfile>,
    pub active_profile_id: Option<String>,
    #[serde(default)]
    pub auto_start: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        let default_profile = TunnelProfile {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Default".to_string(),
            target_host: String::new(),
            target_user: String::new(),
            password: None,
            ssh_port: 22,
            socks_port: 1080,
            http_proxy_port: 1081,
            ssh_key_path: None,
            auto_proxy: true,
            auto_reconnect: true,
            test_url: None,
        };
        let id = default_profile.id.clone();
        Self {
            profiles: vec![default_profile],
            active_profile_id: Some(id),
            auto_start: false,
        }
    }
}

impl AppConfig {
    pub fn active_profile(&self) -> Option<&TunnelProfile> {
        self.active_profile_id
            .as_ref()
            .and_then(|id| self.profiles.iter().find(|p| &p.id == id))
    }

    pub fn active_profile_mut(&mut self) -> Option<&mut TunnelProfile> {
        self.active_profile_id
            .as_ref()
            .and_then(|id| self.profiles.iter_mut().find(|p| &p.id == id))
    }
}
