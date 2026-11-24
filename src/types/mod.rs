//! Data types of the companion protocol.

pub mod contact;
pub mod device;
pub mod message;
pub mod remote;
pub mod rxlog;
pub mod stats;

pub use contact::{
    Contact, ContactList, ContactType, Destination, PUBLIC_KEY_LEN, PUBLIC_KEY_PREFIX_LEN,
    PublicKey, contact_flags,
};
pub use device::{
    AdvertPath, AutoAddConfig, BatteryStatus, ChannelInfo, CustomVars, DefaultFloodScope,
    DeviceInfo, FreqRange, OtherParams, PrivateKey, SelfInfo, TuningParams, autoadd_flags,
    channel_secret_from_name,
};
pub use message::{
    Ack, ChannelData, ChannelMessage, ChannelRxInfo, ContactMessage, ReceivedMessage, SentInfo,
    TextType,
};
pub use remote::{
    AclEntry, AclResponse, AnonOwner, BinaryResponse, ControlData, DiscoverResponse, LoginFailed,
    LoginSuccess, MmaEntry, MmaResponse, Neighbour, NeighboursQuery, NeighboursResponse, OwnerInfo,
    PathDiscovery, RawData, SensorSubscription, TelemetryResponse, TraceData, TraceHop,
};
pub use rxlog::{
    AdvertInfo, DecryptedGroupText, GroupText, PacketContent, PacketInfo, PayloadType, RouteType,
    RxLogData,
};
pub use stats::{CoreStats, PacketStats, RadioStats, StatusResponse};
