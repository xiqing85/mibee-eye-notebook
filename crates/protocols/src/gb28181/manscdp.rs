//! MANSCDP+xml structures for GB/T 28181 protocol.
//!
//! This module provides serde-based structures for parsing and serializing
//! MANSCDP (Monitoring System Protocol and Data Protocol) XML messages
//! used in GB/T 28181-2022 for device catalog, device info, and keepalive.

use serde::{Deserialize, Serialize};

/// Query — platform sends to device (Catalog, DeviceInfo, Keepalive)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "Query")]
pub struct Query {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: String,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
}

/// Response — device sends back to platform (Catalog, DeviceInfo)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct Response {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: String,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// SumNum for Catalog response
    #[serde(rename = "SumNum", skip_serializing_if = "Option::is_none")]
    pub sum_num: Option<u32>,
    /// DeviceList for Catalog response
    #[serde(rename = "DeviceList", skip_serializing_if = "Option::is_none")]
    pub device_list: Option<DeviceList>,
    /// Device for DeviceInfo response
    #[serde(rename = "Device", skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceItem>,
}

/// Device list container for Catalog response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceList {
    #[serde(rename = "Item")]
    pub item: Vec<ChannelItem>,
}

/// CatalogItem — per GB/T 28181-2022 Annex A.2.1 mandatory fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelItem {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Manufacturer")]
    pub manufacturer: String,
    #[serde(rename = "Model")]
    pub model: String,
    #[serde(rename = "Owner")]
    pub owner: String,
    #[serde(rename = "CivilCode")]
    pub civil_code: String,
    #[serde(rename = "Address")]
    pub address: String,
    #[serde(rename = "Parental")]
    pub parental: u32,
    #[serde(rename = "ParentID")]
    pub parent_id: String,
    #[serde(rename = "SafetyWay")]
    pub safety_way: u32,
    #[serde(rename = "RegisterWay")]
    pub register_way: u32,
    #[serde(rename = "Secrecy")]
    pub secrecy: u32,
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "IPAddress")]
    pub ip_address: String,
    #[serde(rename = "Port")]
    pub port: u16,
    #[serde(rename = "Longitude")]
    pub longitude: f64,
    #[serde(rename = "Latitude")]
    pub latitude: f64,
}

/// DeviceItem — for DeviceInfo response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceItem {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Manufacturer")]
    pub manufacturer: String,
    #[serde(rename = "Model")]
    pub model: String,
    #[serde(rename = "Firmware")]
    pub firmware: String,
}

/// Notify — for Keepalive and other notifications
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct Notify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: String,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Status", skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_item_serialize() {
        let item = ChannelItem {
            device_id: "31011500991320000001".to_string(),
            name: "Camera 1".to_string(),
            manufacturer: "MiBee".to_string(),
            model: "Mibee-Cam-01".to_string(),
            owner: "Admin".to_string(),
            civil_code: "310115".to_string(),
            address: "Test Location".to_string(),
            parental: 0,
            parent_id: "31011500991320000000".to_string(),
            safety_way: 0,
            register_way: 1,
            secrecy: 0,
            status: "ON".to_string(),
            ip_address: "192.168.1.100".to_string(),
            port: 5060,
            longitude: 121.4737,
            latitude: 31.2304,
        };

        let xml = serde_xml_rs::to_string(&item).unwrap();
        assert!(xml.contains("<DeviceID>"));
        assert!(xml.contains("31011500991320000001"));
    }

    #[test]
    fn test_device_item_serialize() {
        let device = DeviceItem {
            device_id: "31011500991320000001".to_string(),
            name: "Mibee Camera".to_string(),
            manufacturer: "MiBee".to_string(),
            model: "Mibee-Cam-01".to_string(),
            firmware: "v1.0.0".to_string(),
        };

        let xml = serde_xml_rs::to_string(&device).unwrap();
        assert!(xml.contains("<DeviceID>"));
        assert!(xml.contains("<Firmware>"));
    }

    #[test]
    fn test_response_catalog_fields() {
        // Verify Response struct fields are correctly defined
        let response = Response {
            cmd_type: "Catalog".to_string(),
            sn: "123".to_string(),
            device_id: "31011500991320000001".to_string(),
            sum_num: Some(1),
            device_list: Some(DeviceList { item: vec![] }),
            device: None,
        };
        assert_eq!(response.cmd_type, "Catalog");
        assert_eq!(response.sum_num, Some(1));
        assert!(response.device_list.is_some());
        assert!(response.device.is_none());
    }
}
