//! Command, response and push codes of the companion protocol.

/// Generates a `u8`-backed enum with `from_code`/`code` conversions.
macro_rules! code_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $value:literal, )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant = $value, )*
        }

        impl $name {
            /// Maps a code byte, or `None` for an unknown code.
            #[must_use]
            pub const fn from_code(code: u8) -> Option<Self> {
                match code {
                    $( $value => Some(Self::$variant), )*
                    _ => None,
                }
            }

            /// The code byte.
            #[must_use]
            pub const fn code(self) -> u8 {
                self as u8
            }
        }

        impl From<$name> for u8 {
            fn from(value: $name) -> u8 {
                value as u8
            }
        }
    };
}

code_enum! {
    /// Commands sent to the device (the first byte of a command frame).
    pub enum CommandType {
        /// `CMD_APP_START`
        AppStart = 1,
        /// `CMD_SEND_TXT_MSG`
        SendTxtMsg = 2,
        /// `CMD_SEND_CHANNEL_TXT_MSG`
        SendChannelTxtMsg = 3,
        /// `CMD_GET_CONTACTS`, with an optional `since` timestamp
        GetContacts = 4,
        /// `CMD_GET_DEVICE_TIME`
        GetDeviceTime = 5,
        /// `CMD_SET_DEVICE_TIME`
        SetDeviceTime = 6,
        /// `CMD_SEND_SELF_ADVERT`
        SendSelfAdvert = 7,
        /// `CMD_SET_ADVERT_NAME`
        SetAdvertName = 8,
        /// `CMD_ADD_UPDATE_CONTACT`
        AddUpdateContact = 9,
        /// `CMD_SYNC_NEXT_MESSAGE`
        SyncNextMessage = 10,
        /// `CMD_SET_RADIO_PARAMS`
        SetRadioParams = 11,
        /// `CMD_SET_RADIO_TX_POWER`
        SetRadioTxPower = 12,
        /// `CMD_RESET_PATH`
        ResetPath = 13,
        /// `CMD_SET_ADVERT_LATLON`
        SetAdvertLatLon = 14,
        /// `CMD_REMOVE_CONTACT`
        RemoveContact = 15,
        /// `CMD_SHARE_CONTACT`
        ShareContact = 16,
        /// `CMD_EXPORT_CONTACT`
        ExportContact = 17,
        /// `CMD_IMPORT_CONTACT`
        ImportContact = 18,
        /// `CMD_REBOOT`
        Reboot = 19,
        /// `CMD_GET_BATT_AND_STORAGE`
        GetBattAndStorage = 20,
        /// `CMD_SET_TUNING_PARAMS`
        SetTuningParams = 21,
        /// `CMD_DEVICE_QUERY`
        DeviceQuery = 22,
        /// `CMD_EXPORT_PRIVATE_KEY`
        ExportPrivateKey = 23,
        /// `CMD_IMPORT_PRIVATE_KEY`
        ImportPrivateKey = 24,
        /// `CMD_SEND_RAW_DATA`
        SendRawData = 25,
        /// `CMD_SEND_LOGIN`
        SendLogin = 26,
        /// `CMD_SEND_STATUS_REQ`
        SendStatusReq = 27,
        /// `CMD_HAS_CONNECTION`
        HasConnection = 28,
        /// `CMD_LOGOUT`
        Logout = 29,
        /// `CMD_GET_CONTACT_BY_KEY`
        GetContactByKey = 30,
        /// `CMD_GET_CHANNEL`
        GetChannel = 31,
        /// `CMD_SET_CHANNEL`
        SetChannel = 32,
        /// `CMD_SIGN_START`
        SignStart = 33,
        /// `CMD_SIGN_DATA`
        SignData = 34,
        /// `CMD_SIGN_FINISH`
        SignFinish = 35,
        /// `CMD_SEND_TRACE_PATH`
        SendTracePath = 36,
        /// `CMD_SET_DEVICE_PIN`
        SetDevicePin = 37,
        /// `CMD_SET_OTHER_PARAMS`
        SetOtherParams = 38,
        /// `CMD_SEND_TELEMETRY_REQ`
        SendTelemetryReq = 39,
        /// `CMD_GET_CUSTOM_VARS`
        GetCustomVars = 40,
        /// `CMD_SET_CUSTOM_VAR`
        SetCustomVar = 41,
        /// `CMD_GET_ADVERT_PATH`
        GetAdvertPath = 42,
        /// `CMD_GET_TUNING_PARAMS`
        GetTuningParams = 43,
        /// `CMD_SEND_BINARY_REQ`
        SendBinaryReq = 50,
        /// `CMD_FACTORY_RESET`
        FactoryReset = 51,
        /// `CMD_SEND_PATH_DISCOVERY_REQ`
        SendPathDiscoveryReq = 52,
        /// `CMD_SET_FLOOD_SCOPE_KEY`
        SetFloodScope = 54,
        /// `CMD_SEND_CONTROL_DATA`
        SendControlData = 55,
        /// `CMD_GET_STATS`
        GetStats = 56,
        /// `CMD_SEND_ANON_REQ`
        SendAnonReq = 57,
        /// `CMD_SET_AUTOADD_CONFIG`
        SetAutoaddConfig = 58,
        /// `CMD_GET_AUTOADD_CONFIG`
        GetAutoaddConfig = 59,
        /// `CMD_GET_ALLOWED_REPEAT_FREQ`
        GetAllowedRepeatFreq = 60,
        /// `CMD_SET_PATH_HASH_MODE`
        SetPathHashMode = 61,
        /// `CMD_SEND_CHANNEL_DATA`
        SendChannelData = 62,
        /// `CMD_SET_DEFAULT_FLOOD_SCOPE`
        SetDefaultFloodScope = 63,
        /// `CMD_GET_DEFAULT_FLOOD_SCOPE`
        GetDefaultFloodScope = 64,
        /// `CMD_SEND_RAW_PACKET`
        SendRawPacket = 65,
        /// `CMD_RUN_CLI_COMMAND` (firmware code 14 and later)
        RunCliCommand = 66,
    }
}

code_enum! {
    /// Frames the device sends: replies (below `0x80`) and pushes (`0x80` and up).
    pub enum PacketType {
        /// `RESP_CODE_OK`
        Ok = 0,
        /// `RESP_CODE_ERR`
        Error = 1,
        /// `RESP_CODE_CONTACTS_START`
        ContactStart = 2,
        /// `RESP_CODE_CONTACT`
        Contact = 3,
        /// `RESP_CODE_END_OF_CONTACTS`
        ContactEnd = 4,
        /// `RESP_CODE_SELF_INFO`
        SelfInfo = 5,
        /// `RESP_CODE_SENT`
        MsgSent = 6,
        /// `RESP_CODE_CONTACT_MSG_RECV`
        ContactMsgRecv = 7,
        /// `RESP_CODE_CHANNEL_MSG_RECV`
        ChannelMsgRecv = 8,
        /// `RESP_CODE_CURR_TIME`
        CurrentTime = 9,
        /// `RESP_CODE_NO_MORE_MESSAGES`
        NoMoreMsgs = 10,
        /// `RESP_CODE_EXPORT_CONTACT`
        ContactUri = 11,
        /// `RESP_CODE_BATT_AND_STORAGE`
        Battery = 12,
        /// `RESP_CODE_DEVICE_INFO`
        DeviceInfo = 13,
        /// `RESP_CODE_PRIVATE_KEY`
        PrivateKey = 14,
        /// `RESP_CODE_DISABLED`
        Disabled = 15,
        /// `RESP_CODE_CONTACT_MSG_RECV_V3`
        ContactMsgRecvV3 = 16,
        /// `RESP_CODE_CHANNEL_MSG_RECV_V3`
        ChannelMsgRecvV3 = 17,
        /// `RESP_CODE_CHANNEL_INFO`
        ChannelInfo = 18,
        /// `RESP_CODE_SIGN_START`
        SignStart = 19,
        /// `RESP_CODE_SIGNATURE`
        Signature = 20,
        /// `RESP_CODE_CUSTOM_VARS`
        CustomVars = 21,
        /// `RESP_CODE_ADVERT_PATH`
        AdvertPath = 22,
        /// `RESP_CODE_TUNING_PARAMS`
        TuningParams = 23,
        /// `RESP_CODE_STATS`
        Stats = 24,
        /// `RESP_CODE_AUTOADD_CONFIG`
        AutoaddConfig = 25,
        /// `RESP_ALLOWED_REPEAT_FREQ`
        AllowedRepeatFreq = 26,
        /// `RESP_CODE_CHANNEL_DATA_RECV`
        ChannelDataRecv = 27,
        /// `RESP_CODE_DEFAULT_FLOOD_SCOPE`
        DefaultFloodScope = 28,
        /// `RESP_CODE_CLI_REPLY`
        CliReply = 29,
        /// `PUSH_CODE_ADVERT`
        Advertisement = 0x80,
        /// `PUSH_CODE_PATH_UPDATED`
        PathUpdate = 0x81,
        /// `PUSH_CODE_SEND_CONFIRMED`
        Ack = 0x82,
        /// `PUSH_CODE_MSG_WAITING`
        MessagesWaiting = 0x83,
        /// `PUSH_CODE_RAW_DATA`
        RawData = 0x84,
        /// `PUSH_CODE_LOGIN_SUCCESS`
        LoginSuccess = 0x85,
        /// `PUSH_CODE_LOGIN_FAIL`
        LoginFailed = 0x86,
        /// `PUSH_CODE_STATUS_RESPONSE`
        StatusResponse = 0x87,
        /// `PUSH_CODE_LOG_RX_DATA`
        LogData = 0x88,
        /// `PUSH_CODE_TRACE_DATA`
        TraceData = 0x89,
        /// `PUSH_CODE_NEW_ADVERT`
        NewAdvert = 0x8A,
        /// `PUSH_CODE_TELEMETRY_RESPONSE`
        TelemetryResponse = 0x8B,
        /// `PUSH_CODE_BINARY_RESPONSE`
        BinaryResponse = 0x8C,
        /// `PUSH_CODE_PATH_DISCOVERY_RESPONSE`
        PathDiscoveryResponse = 0x8D,
        /// `PUSH_CODE_CONTROL_DATA`
        ControlData = 0x8E,
        /// `PUSH_CODE_CONTACT_DELETED`
        ContactDeleted = 0x8F,
        /// `PUSH_CODE_CONTACTS_FULL`
        ContactsFull = 0x90,
    }
}

impl PacketType {
    /// True for pushes, which the device sends on its own.
    #[must_use]
    pub const fn is_push(self) -> bool {
        self.code() >= 0x80
    }
}

code_enum! {
    /// Request types of `CMD_SEND_BINARY_REQ`.
    pub enum BinaryReqType {
        /// Status of a repeater, room server or sensor.
        Status = 0x01,
        /// Keep-alive.
        KeepAlive = 0x02,
        /// Telemetry (Cayenne LPP).
        Telemetry = 0x03,
        /// Min/max/average series of a sensor.
        Mma = 0x04,
        /// Access control list.
        Acl = 0x05,
        /// Neighbours of a repeater.
        Neighbours = 0x06,
        /// Firmware version, name and owner of a repeater (firmware level 2 and later).
        OwnerInfo = 0x07,
        /// Subscribe to a sensor's telemetry pushes.
        Subscribe = 0x08,
        /// End a subscription to a sensor's telemetry pushes.
        Unsubscribe = 0x09,
    }
}

code_enum! {
    /// Request types of `CMD_SEND_ANON_REQ`.
    pub enum AnonReqType {
        /// Regions the node serves.
        Regions = 0x01,
        /// Name and owner info.
        Owner = 0x02,
        /// Basic info (the remote clock).
        Basic = 0x03,
    }
}

code_enum! {
    /// Control data types (`CMD_SEND_CONTROL_DATA`, `PUSH_CODE_CONTROL_DATA`).
    pub enum ControlType {
        /// Node discovery request; the low bit asks for key prefixes only.
        NodeDiscoverReq = 0x80,
        /// Node discovery response; the low nibble is the node type.
        NodeDiscoverResp = 0x90,
    }
}

code_enum! {
    /// Statistics groups of `CMD_GET_STATS`.
    pub enum StatsType {
        /// Battery, uptime, error flags, queue length.
        Core = 0,
        /// Noise floor, last RSSI/SNR, airtime.
        Radio = 1,
        /// Packet counters.
        Packets = 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_codes_match_the_firmware() {
        assert_eq!(CommandType::AppStart.code(), 1);
        assert_eq!(CommandType::GetTuningParams.code(), 43);
        assert_eq!(CommandType::SendBinaryReq.code(), 50);
        assert_eq!(CommandType::SetFloodScope.code(), 54);
        assert_eq!(CommandType::GetStats.code(), 56);
        assert_eq!(CommandType::RunCliCommand.code(), 66);
        assert_eq!(CommandType::from_code(53), None);
    }

    #[test]
    fn packet_types_cover_replies_and_pushes() {
        assert_eq!(PacketType::from_code(29), Some(PacketType::CliReply));
        assert_eq!(PacketType::from_code(0x90), Some(PacketType::ContactsFull));
        // 0x32 (50) is a command code (CMD_SEND_BINARY_REQ), not a reply.
        assert_eq!(PacketType::from_code(0x32), None);
        assert!(PacketType::Ack.is_push());
        assert!(!PacketType::Ok.is_push());
    }
}
