use crate::models::{PlayerXml, ResourcesXml};

pub const HTTP_PORT: u16 = 32500;

pub struct Player {
    pub device_id: String,
    pub name: String,
    pub product: String,
    pub version: String,
    pub protocol_version: String,
    pub protocol_capabilities: String,
    pub device_class: String,
    pub platform: String,
    pub platform_version: String,
}

impl Player {
    pub fn new(name: String, product: String) -> Self {
        Self {
            device_id: uuid_like(),
            name,
            product,
            version: "1.0.0".to_string(),
            protocol_version: "1".to_string(),
            protocol_capabilities: "timeline,playback,playqueues,playqueues-creation".to_string(),
            device_class: "pc".to_string(),
            platform: std::env::consts::OS.to_string(),
            platform_version: String::new(),
        }
    }

    pub fn gdm_reply(&self) -> String {
        format!(
            "HTTP/1.0 200 OK\r\n\
             Content-Type: plex/media-player\r\n\
             Port: {HTTP_PORT}\r\n\
             Protocol: plex\r\n\
             Resource-Identifier: {}\r\n\
             Name: {}\r\n\
             Version: {}\r\n\
             Product: {}\r\n\
             Protocol-Version: {}\r\n\
             Protocol-Capabilities: {}\r\n\
             Device-Class: {}\r\n",
            self.device_id,
            self.name,
            self.version,
            self.product,
            self.protocol_version,
            self.protocol_capabilities,
            self.device_class,
        )
    }

    pub fn resources_xml(&self) -> String {
        let xml = ResourcesXml {
            size: 1,
            player: PlayerXml {
                machine_identifier: self.device_id.clone(),
                device_class: self.device_class.clone(),
                platform: self.platform.clone(),
                platform_version: self.platform_version.clone(),
                product: self.product.clone(),
                protocol: "plex".to_string(),
                protocol_version: self.protocol_version.clone(),
                protocol_capabilities: self.protocol_capabilities.clone(),
                title: self.name.clone(),
                version: self.version.clone(),
            },
        };
        serde_xml_rs::to_string(&xml).unwrap_or_default()
    }
}

fn uuid_like() -> String {
    let host = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    let h: u64 = host.bytes().fold(0xcbf29ce484222325u64, |acc, b| {
        (acc ^ b as u64).wrapping_mul(0x100000001b3)
    });
    format!("{h:016x}000000000000000000000000")
}
