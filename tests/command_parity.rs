//! Parity with meshcore_py in what goes out: the same calls through both
//! libraries, on devices that answer alike, write the same frames
//! (`tests/fixtures/meshcore_py_commands.jsonl`, recorded with
//! `meshcore_py_commands.py`), apart from the deviations listed below.

mod common;

use std::sync::{Arc, Mutex};

use common::{client_on_device, device_info_frame, self_info_frame, status_fields};
use meshcore::{
    BinaryReqType, Contact, ContactType, FloodScope, MeshCore, NeighboursQuery, Options,
    OtherParams, PathLen, PublicKey, RequestOptions, RetryOptions, TraceRequest,
};

/// A repeater with a path of one 2-byte hop.
const KEY: [u8; 32] = [0x11; 32];
/// A companion that floods.
const KEY2: [u8; 32] = [0x33; 32];
/// A node that is no contact.
const STRANGER: [u8; 32] = [0x55; 32];
/// The time the calls give.
const T: u32 = 1_700_000_000;

/// Where this library sends something else, as (case, what meshcore_py
/// sends, what this library sends), each frame as hex. The README lists
/// these differences.
const DEVIATIONS: &[(&str, &[&str], &[&str])] = &[
    // The factory reset carries the text the firmware requires.
    ("factory_reset", &["33"], &["337265736574"]),
    // Frequency and bandwidth are rounded; meshcore_py cuts off the rest after a
    // float multiplication.
    (
        "set_radio_rounding",
        &["0b56440d0024f400000908"],
        &["0b57440d0024f400000908"],
    ),
    (
        "set_radio_rounding_512",
        &["0b01d0070024f400000908"],
        &["0b02d0070024f400000908"],
    ),
    (
        "set_radio_rounding_bw",
        &["0bd39e0600401f00000908"],
        &["0bd39e0600411f00000908"],
    ),
    // A negative TX power goes out; meshcore_py cannot encode it.
    ("set_tx_power_negative", &[], &["0cf7ffffff"]),
    // Names are cut to the 31 bytes the firmware keeps.
    (
        "set_name_long",
        &["086e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e"],
        &["086e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e"],
    ),
    (
        "set_channel_long_name",
        &[concat!(
            "200123616161616161616161616161616161616161616161616161616161616161610e14",
            "41ff411b8f07a5eb893749ca694c"
        )],
        &[concat!(
            "200123616161616161616161616161616161616161616161616161616161616161000e14",
            "41ff411b8f07a5eb893749ca694c"
        )],
    ),
    (
        "add_contact_long_name",
        &[concat!(
            "0944444444444444444444444444444444444444444444444444444444444444440200ff",
            "000000000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000000000006e6e6e6e6e6e6e6e",
            "6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e00000000",
            "0000000000000000"
        )],
        &[concat!(
            "0944444444444444444444444444444444444444444444444444444444444444440200ff",
            "000000000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000000000006e6e6e6e6e6e6e6e",
            "6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e00000000000000000000000000"
        )],
    ),
    // A trace path that is empty, no whole number of hops or longer than 64
    // hops is refused.
    ("trace_empty", &["2408000000000000000000"], &[]),
    ("trace_not_whole_hops", &["24090000000000000001aabbcc"], &[]),
    (
        "trace_too_many_hops",
        &[concat!(
            "240a0000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaa"
        )],
        &[],
    ),
    // Raw data has hop count and hash mode in its length byte, as firmware 1.18
    // reads it.
    (
        "raw_data_two_byte_hashes",
        &["1904a1a2b1b201020304"],
        &["1942a1a2b1b201020304"],
    ),
    // A path its length byte cannot describe is refused.
    (
        "change_path_not_whole_hops",
        &[concat!(
            "091111111111111111111111111111111111111111111111111111111111111111020041",
            "a1b2c3000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000000000005265700000000000",
            "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800"
        )],
        &[],
    ),
    ("channel_data_not_whole_hops", &["3e01410102033412aa"], &[]),
    (
        "change_path_too_many_hops",
        &[concat!(
            "091111111111111111111111111111111111111111111111111111111111111111020040",
            "070707070707070707070707070707070707070707070707070707070707070707070707",
            "070707070707070707070707070707070707070707070707070707075265700000000000",
            "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800"
        )],
        &[],
    ),
    (
        "change_path_too_long",
        &[concat!(
            "091111111111111111111111111111111111111111111111111111111111111111020061",
            "070707070707070707070707070707070707070707070707070707070707070707070707",
            "070707070707070707070707070707070707070707070707070707070707526570000000",
            "0000000000000000000000000000000000000000000000000000e8030000fc1f3103e47d",
            "9800"
        )],
        &[],
    ),
    (
        "change_path_mode_3",
        &[concat!(
            "0911111111111111111111111111111111111111111111111111111111111111110200c1",
            "a1b2c3d40000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000000000005265700000000000",
            "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800"
        )],
        &[],
    ),
    (
        "channel_data_too_many_hops",
        &[concat!(
            "3e0140070707070707070707070707070707070707070707070707070707070707070707",
            "070707070707070707070707070707070707070707070707070707070707073412aa"
        )],
        &[],
    ),
    ("channel_data_mode_3", &["3e01c1010203043412aa"], &[]),
    (
        "raw_data_too_many_hops",
        &[concat!(
            "194007070707070707070707070707070707070707070707070707070707070707070707",
            "07070707070707070707070707070707070707070707070707070707070701020304"
        )],
        &[],
    ),
    (
        "update_contact_path_mismatch",
        &[concat!(
            "091111111111111111111111111111111111111111111111111111111111111111020002",
            "010000000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000000000005265700000000000",
            "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800"
        )],
        &[],
    ),
    (
        "update_contact_flood_with_path",
        &[concat!(
            "0911111111111111111111111111111111111111111111111111111111111111110200ff",
            "010000000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000000000005265700000000000",
            "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800"
        )],
        &[],
    ),
    (
        "update_contact_path_too_long",
        &[concat!(
            "091111111111111111111111111111111111111111111111111111111111111111020061",
            "010101010101010101010101010101010101010101010101010101010101010101010101",
            "010101010101010101010101010101010101010101010101010101010101526570000000",
            "0000000000000000000000000000000000000000000000000000e8030000fc1f3103e47d",
            "9800"
        )],
        &[],
    ),
    // With the lastmod 0 the device would list the contact nowhere.
    (
        "update_contact_lastmod_0",
        &[concat!(
            "091111111111111111111111111111111111111111111111111111111111111111020041",
            "a1a200000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000000000005265700000000000",
            "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800",
            "00000000"
        )],
        &[],
    ),
    // The node type of a bare key comes from the contact cache.
    (
        "send_cmd_bare_key",
        &["02010000f15365333333333333766572"],
        &["02030000f15365333333333333766572"],
    ),
    // Retries to a key prefix no contact has go on without the path reset.
    (
        "retry_unknown_prefix",
        &[
            "02000000f153655555555555556869",
            "02000100f153655555555555556869",
            "04",
        ],
        &[
            "02000000f153655555555555556869",
            "02000100f153655555555555556869",
            "04",
            "02000200f153655555555555556869",
        ],
    ),
    // A path without hash mode fails when the device does not tell its own;
    // meshcore_py takes mode 0.
    (
        "change_path_hash_mode_refused",
        &[
            "1603",
            concat!(
                "091111111111111111111111111111111111111111111111111111111111111111020004",
                "a1b2c3d40000000000000000000000000000000000000000000000000000000000000000",
                "000000000000000000000000000000000000000000000000000000005265700000000000",
                "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800"
            ),
        ],
        &["1603"],
    ),
    // An anonymous request takes its reply path from the device's own record.
    (
        "anon_unknown_node",
        &["3955555555555555555555555555555555555555555555555555555555555555550100"],
        &[
            "1e5555555555555555555555555555555555555555555555555555555555555555",
            "3955555555555555555555555555555555555555555555555555555555555555550100",
        ],
    ),
    (
        "anon_contact_with_path",
        &["3911111111111111111111111111111111111111111111111111111111111111110141a1a2"],
        &[
            "1e1111111111111111111111111111111111111111111111111111111111111111",
            "3911111111111111111111111111111111111111111111111111111111111111110141a1a2",
        ],
    ),
    // A contact set to zero-hop for one goes back to flood also when setting
    // zero-hop got no answer.
    (
        "anon_zero_hop_unanswered",
        &[
            "1e3333333333333333333333333333333333333333333333333333333333333333",
            "1603",
            concat!(
                "093333333333333333333333333333333333333333333333333333333333333333010040",
                "000000000000000000000000000000000000000000000000000000000000000000000000",
                "000000000000000000000000000000000000000000000000000000004368617400000000",
                "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800"
            ),
        ],
        &[
            "1e3333333333333333333333333333333333333333333333333333333333333333",
            "1603",
            concat!(
                "093333333333333333333333333333333333333333333333333333333333333333010040",
                "000000000000000000000000000000000000000000000000000000000000000000000000",
                "000000000000000000000000000000000000000000000000000000004368617400000000",
                "000000000000000000000000000000000000000000000000e8030000fc1f3103e47d9800"
            ),
            "0d3333333333333333333333333333333333333333333333333333333333333333",
        ],
    ),
    // Input the firmware answers with ERR_CODE_ILLEGAL_ARG is refused.
    ("devicepin_illegal", &["2539300000"], &[]),
    ("coords_out_of_range", &["0ec08c6c050000000000000000"], &[]),
    ("radio_out_of_range", &["0bd39e060024f400000408"], &[]),
    ("tx_power_above_max", &["0c17000000"], &[]),
    ("path_hash_mode_reserved", &["3d0003"], &[]),
    ("custom_var_without_name", &["293a31"], &[]),
    // A text the firmware would not send whole, and input it would read
    // otherwise or take for an unknown command, are refused.
    ("custom_var_colon", &["29613a623a63"], &[]),
    ("empty_message", &["02000000f15365111111111111"], &[]),
    (
        "message_too_long",
        &[concat!(
            "02000000f153651111111111117878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878"
        )],
        &[],
    ),
    ("message_nul", &["02000000f15365111111111111610062"], &[]),
    (
        "channel_message_too_long",
        &[concat!(
            "03000000f153657878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878"
        )],
        &[],
    ),
    ("empty_name", &["08"], &[]),
    ("short_cli_command", &["4276"], &[]),
    ("empty_sign_chunk", &["22"], &[]),
    ("short_raw_packet", &["410011"], &[]),
    (
        "short_contact_card",
        &[concat!(
            "12aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )],
        &[],
    ),
    ("control_type_without_top_bit", &["371001"], &[]),
    (
        "long_password",
        &[concat!(
            "1a1111111111111111111111111111111111111111111111111111111111111111707070",
            "70707070707070707070707070"
        )],
        &[],
    ),
    (
        "fifth_attempt_too_long",
        &[concat!(
            "02000400f153651111111111117878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "78787878787878787878787878787878787878787878787878787878"
        )],
        &[],
    ),
    ("empty_node_command", &["02010000f15365111111111111"], &[]),
    (
        "node_command_too_long",
        &[concat!(
            "02010000f153651111111111117878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878787878787878",
            "787878787878787878787878787878787878787878787878787878787878"
        )],
        &[],
    ),
    ("custom_var_too_short", &["29613a"], &[]),
    (
        "node_command_nul",
        &["02010000f15365111111111111760072"],
        &[],
    ),
    ("channel_message_nul", &["03000000f15365610062"], &[]),
    ("cli_command_nul", &["4276650072"], &[]),
    ("name_nul", &["08610062"], &[]),
    (
        "contact_name_nul",
        &[concat!(
            "0944444444444444444444444444444444444444444444444444444444444444440200ff",
            "000000000000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000000000000006100620000000000",
            "000000000000000000000000000000000000000000000000000000000000000000000000"
        )],
        &[],
    ),
    (
        "channel_name_nul",
        &[concat!(
            "20012361006200000000000000000000000000000000000000000000000000000000be50",
            "862227f5ab13176fe412a04969e3"
        )],
        &[],
    ),
    ("custom_var_nul", &["296770733a3100"], &[]),
    (
        "default_flood_scope_nul",
        &[concat!(
            "3f23610062000000000000000000000000000000000000000000000000000000be508622",
            "27f5ab13176fe412a04969e3"
        )],
        &[],
    ),
    (
        "password_nul",
        &["1a11111111111111111111111111111111111111111111111111111111111111117061007373"],
        &[],
    ),
    // A command longer than the firmware takes is refused.
    (
        "command_too_long",
        &[concat!(
            "12aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )],
        &[],
    ),
];

/// The cases in which meshcore_py raises an error, with its name.
const RAISED: &[(&str, &str)] = &[
    // values it cannot encode
    ("set_tx_power_negative", "OverflowError"),
    ("tx_power_below_min", "OverflowError"),
    ("radio_negative_frequency", "OverflowError"),
    ("radio_negative_bandwidth", "OverflowError"),
    // a hash mode that spills out of the length byte
    ("change_path_mode_4", "OverflowError"),
    ("channel_data_mode_4", "ValueError"),
    // the path reset for a key prefix no contact has
    ("retry_unknown_prefix", "ValueError"),
];

/// A contact the device holds.
fn contact_frame(key: [u8; 32]) -> Option<Vec<u8>> {
    let (kind, name, path_len, path): (u8, &[u8], u8, &[u8]) = match key {
        KEY => (2, b"Rep", 0x41, &[0xa1, 0xa2]),
        KEY2 => (1, b"Chat", 0xff, &[]),
        _ => return None,
    };
    let mut path_field = [0u8; 64];
    path_field[..path.len()].copy_from_slice(path);
    let mut name_field = [0u8; 32];
    name_field[..name.len()].copy_from_slice(name);
    Some(
        [
            &[3][..],
            &key,
            &[kind, 0, path_len],
            &path_field,
            &name_field,
            &1000u32.to_le_bytes(),
            &53_551_100i32.to_le_bytes(),
            &9_993_700i32.to_le_bytes(),
            &2000u32.to_le_bytes(),
        ]
        .concat(),
    )
}

/// A neighbours page: three neighbours, two in the first page.
fn neighbours(cmd: &[u8]) -> Vec<u8> {
    let offset = u16::from_le_bytes([cmd[36], cmd[37]]);
    let count: u8 = if offset == 0 { 2 } else { 1 };
    let mut data = 3i16.to_le_bytes().to_vec();
    data.extend(i16::from(count).to_le_bytes());
    for n in 0..count {
        data.extend(vec![0xa0 + n; usize::from(cmd[39])]);
        data.extend(60i32.to_le_bytes());
        data.push(20);
    }
    data
}

/// The answer of a remote node to a binary request.
fn answer(cmd: &[u8]) -> Vec<u8> {
    match cmd[33] {
        1 => status_fields(4100),
        3 => vec![1, 116, 0x01, 0x9c],
        4 => [&T.to_le_bytes()[..], &[1, 103, 0, 200, 0, 255, 0, 221]].concat(),
        5 => [&KEY[..6], &[3], &[0; 7]].concat(),
        6 => neighbours(cmd),
        7 => b"v1.18.0\nRep\nOwner\0\0".to_vec(),
        8 => [&3600u16.to_le_bytes()[..], b"\0\0#hamburg\0"].concat(),
        _ => Vec::new(),
    }
}

fn stats(kind: u8) -> Vec<u8> {
    match kind {
        0 => [
            &[24, 0][..],
            &4100u16.to_le_bytes(),
            &3600u32.to_le_bytes(),
            &0u16.to_le_bytes(),
            &[2],
        ]
        .concat(),
        1 => [
            &[24, 1][..],
            &(-110i16).to_le_bytes(),
            &[0xb0, 26],
            &10u32.to_le_bytes(),
            &20u32.to_le_bytes(),
        ]
        .concat(),
        _ => {
            let counters: Vec<u8> = (1..8u32).flat_map(u32::to_le_bytes).collect();
            [&[24, 2][..], &counters].concat()
        }
    }
}

/// The frames the device sends for a command, as the stand-in of
/// `meshcore_py_commands.py` does (which sends the answers from the mesh a
/// moment later).
fn respond(cmd: &[u8]) -> Vec<Vec<u8>> {
    let sent = |tag: u32| [&[6, 0][..], &tag.to_le_bytes(), &100u32.to_le_bytes()].concat();
    let binary = |tag: u32, data: &[u8]| [&[0x8c, 0][..], &tag.to_le_bytes(), data].concat();
    let with = |code: u8, data: &[u8]| [&[code][..], data].concat();
    match cmd[0] {
        1 => vec![self_info_frame("Node")],
        22 => vec![device_info_frame(1)],
        2 => vec![sent(0xa000_0000 + u32::from(cmd[2]))],
        4 => {
            let mut frames = vec![with(2, &2u32.to_le_bytes())];
            frames.extend([KEY, KEY2].map(|key| contact_frame(key).unwrap()));
            frames.push(with(4, &5000u32.to_le_bytes()));
            frames
        }
        5 => vec![with(9, &T.to_le_bytes())],
        10 => vec![vec![10]],
        17 => vec![vec![11, 0xaa, 0xbb]],
        19 => Vec::new(),
        20 => vec![
            [
                &[12][..],
                &3540u16.to_le_bytes(),
                &1024u32.to_le_bytes(),
                &4096u32.to_le_bytes(),
            ]
            .concat(),
        ],
        23 => vec![with(14, &[0xab; 64])],
        26 => vec![
            sent(0x1a1a_1a1a),
            [&[0x85, 1][..], &cmd[1..7], &T.to_le_bytes(), &[3, 2]].concat(),
        ],
        27 | 52 => vec![sent(0x1b00_0000 + u32::from(cmd[0]))],
        30 => {
            let key = cmd[1..33].try_into().unwrap();
            vec![contact_frame(key).unwrap_or_else(|| vec![1, 2])]
        }
        31 => {
            let mut name = [0u8; 32];
            name[..5].copy_from_slice(b"#chan");
            vec![[&[18, cmd[1]][..], &name, &[cmd[1]; 16]].concat()]
        }
        33 => vec![with(19, &[&[0][..], &8192u32.to_le_bytes()].concat())],
        35 => vec![with(20, &[0x5a; 64])],
        36 => vec![sent(u32::from_le_bytes(cmd[1..5].try_into().unwrap()))],
        // the device's own telemetry, with its own key prefix
        39 if cmd.len() == 4 => vec![[&[0x8b, 0][..], &[0x22; 6], &[1, 116, 0x01, 0x9c]].concat()],
        39 => vec![sent(0x2700_0000)],
        40 => vec![with(21, b"gps:1")],
        42 => vec![[&[22][..], &T.to_le_bytes(), &[0x41, 0xaa, 0xbb]].concat()],
        43 => vec![[&[23][..], &500u32.to_le_bytes(), &1200u32.to_le_bytes()].concat()],
        50 => {
            let kind = cmd[33];
            let page = if kind == 6 {
                u32::from(u16::from_le_bytes([cmd[36], cmd[37]])) << 8
            } else {
                0
            };
            let tag = 0xa100_0000 + u32::from(kind) + page;
            vec![sent(tag), binary(tag, &answer(cmd))]
        }
        56 => vec![stats(cmd[1])],
        57 => {
            let tag = 0xa200_0000 + u32::from(cmd[33]);
            let text: &[u8] = if cmd[33] == 2 {
                b"Rep\nOwner\0"
            } else {
                b"answer"
            };
            vec![
                sent(tag),
                binary(tag, &[&T.to_le_bytes()[..], text].concat()),
            ]
        }
        59 => vec![vec![25, 0x0e, 3]],
        60 => vec![
            [
                &[26][..],
                &433_000u32.to_le_bytes(),
                &434_000u32.to_le_bytes(),
            ]
            .concat(),
        ],
        64 => {
            let mut name = [0u8; 31];
            name[..8].copy_from_slice(b"#hamburg");
            vec![[&[28][..], &name, &[5; 16]].concat()]
        }
        66 => vec![with(29, b"> v1.18.0")],
        _ => vec![vec![0]],
    }
}

/// A contact as meshcore_py's CLI builds one to add.
fn new_contact(name: &str) -> Contact {
    Contact::new(PublicKey::new([0x44; 32]), ContactType::Repeater, name)
}

fn trace(tag: u32, flags: Option<u8>, path: &[u8]) -> TraceRequest {
    TraceRequest {
        tag: Some(tag),
        flags,
        path: path.to_vec(),
        ..TraceRequest::default()
    }
}

/// The calls of a case, as `meshcore_py_commands.py` makes them.
#[allow(clippy::too_many_lines)]
async fn act(case: &str, mc: &MeshCore) {
    let key = PublicKey::new(KEY);
    let chat = PublicKey::new(KEY2);
    let options = RequestOptions::default();
    let cached = |key: &PublicKey| mc.contact(key).unwrap();
    let retry = RetryOptions {
        timestamp: Some(T),
        ..RetryOptions::default()
    };
    let sixteen: [u8; 16] = core::array::from_fn(|n| n as u8);
    match case {
        // the device
        "appstart" => drop(mc.send_appstart().await),
        "device_query" => drop(mc.send_device_query().await),
        "get_bat" => drop(mc.get_bat().await),
        "get_time" => drop(mc.get_time().await),
        "set_time" => drop(mc.set_time(T).await),
        "reboot" => drop(mc.reboot().await),
        "advert" => drop(mc.send_advert(false).await),
        "advert_flood" => drop(mc.send_advert(true).await),
        "set_name" => drop(mc.set_name("Node 2").await),
        "set_coords" => drop(mc.set_coords(53.5511, 9.9937).await),
        "set_coords_south_west" => drop(mc.set_coords(-33.8688, -151.2093).await),
        "set_tx_power" => drop(mc.set_tx_power(20).await),
        "set_radio" => drop(mc.set_radio(433.875, 62.5, 9, 8, None).await),
        "set_radio_repeat" => drop(mc.set_radio(433.875, 62.5, 9, 8, Some(true)).await),
        "set_tuning" => drop(mc.set_tuning(500, 1200).await),
        "get_tuning" => drop(mc.get_tuning().await),
        "set_other_params" => {
            let params = OtherParams {
                manual_add_contacts: true,
                telemetry_mode_base: 2,
                telemetry_mode_loc: 1,
                telemetry_mode_env: 0,
                adv_loc_policy: 1,
                multi_acks: 3,
            };
            drop(mc.set_other_params(&params).await);
        }
        "single_setters" => {
            drop(mc.set_multi_acks(2).await);
            drop(mc.set_telemetry_mode_base(1).await);
            drop(mc.set_telemetry_mode_loc(2).await);
            drop(mc.set_telemetry_mode_env(1).await);
            drop(mc.set_manual_add_contacts(true).await);
            drop(mc.set_advert_loc_policy(0).await);
        }
        "set_devicepin" => drop(mc.set_devicepin(123_456).await),
        "self_telemetry" => drop(mc.get_self_telemetry().await),
        "custom_vars" => drop(mc.get_custom_vars().await),
        "set_custom_var" => drop(mc.set_custom_var("gps", "1").await),
        "get_channel" => drop(mc.get_channel(2).await),
        "set_hashtag_channel" => drop(mc.set_channel(1, "#hamburg", None).await),
        "set_private_channel" => drop(mc.set_channel(2, "Private", Some(sixteen)).await),
        "export_private_key" => drop(mc.export_private_key().await),
        "import_private_key" => drop(mc.import_private_key(&[0xab; 64]).await),
        "sign" => drop(mc.sign(&(0..=249).collect::<Vec<u8>>(), 120, None).await),
        "has_connection" => drop(mc.has_connection(&key).await),
        "stats_core" => drop(mc.get_stats_core().await),
        "stats_radio" => drop(mc.get_stats_radio().await),
        "stats_packets" => drop(mc.get_stats_packets().await),
        "allowed_repeat_freq" => drop(mc.get_allowed_repeat_freq().await),
        "set_path_hash_mode" => drop(mc.set_path_hash_mode(1).await),
        "get_path_hash_mode" => drop(mc.get_path_hash_mode().await),
        "cli_command" => drop(mc.run_cli_command("ver").await),
        "raw_packet" => drop(mc.send_raw_packet(&[0x11, 0, 9, 9]).await),
        "raw_packet_priority" => {
            drop(mc.send_raw_packet_with_priority(&[0x11, 0, 9, 9], 2).await);
        }
        "factory_reset" => drop(mc.confirm_factory_reset(&mc.request_factory_reset()).await),
        // contacts
        "get_contacts" => drop(mc.get_contacts(0).await),
        "get_contacts_since" => drop(mc.get_contacts(5000).await),
        "get_contacts_async" => drop(mc.get_contacts_async(0).await),
        "ensure_contacts" => drop(mc.ensure_contacts(false).await),
        "reset_path" => drop(mc.reset_path(&key).await),
        "share_contact" => drop(mc.share_contact(&key).await),
        "export_self" => drop(mc.export_contact(None).await),
        "export_contact" => drop(mc.export_contact(Some(&key)).await),
        "import_contact" => drop(mc.import_contact(&[0xaa; 98]).await),
        "remove_contact" => drop(mc.remove_contact(&key).await),
        "update_contact" => drop(mc.update_contact(&cached(&key), None).await),
        "update_contact_lastmod" => drop(mc.update_contact(&cached(&key), Some(7000)).await),
        "update_contact_lastmod_0" => drop(mc.update_contact(&cached(&key), Some(0)).await),
        "add_contact" => drop(mc.add_contact(&new_contact("New")).await),
        // the latter with a device that refuses the hash mode query
        "change_path_device_mode" | "change_path_hash_mode_refused" => {
            let path = [0xa1, 0xb2, 0xc3, 0xd4];
            drop(mc.change_contact_path(&cached(&key), &path, None).await);
        }
        "change_path_given_mode" => {
            let path = [0xa1, 0xb2, 0xc3];
            drop(mc.change_contact_path(&cached(&key), &path, Some(0)).await);
        }
        "change_path_mode_in_text" => {
            let (path, mode) = meshcore::protocol::parse_path_hex("a1b2c3d4e5f6:2").unwrap();
            drop(mc.change_contact_path(&cached(&key), &path, mode).await);
        }
        "change_path_zero_hop" => drop(mc.change_contact_path(&cached(&key), &[], Some(0)).await),
        "change_flags" => drop(mc.change_contact_flags(&cached(&key), 1).await),
        "autoadd" => drop(mc.set_autoadd_config(0x02, None).await),
        "autoadd_hops" => drop(mc.set_autoadd_config(0x0e, Some(3)).await),
        "get_autoadd" => drop(mc.get_autoadd_config().await),
        "contact_by_key" => drop(mc.get_contact_by_key(&key).await),
        "advert_path" => drop(mc.get_advert_path(&key).await),
        // messages
        "get_msg" => drop(mc.get_msg().await),
        "auto_fetch" => mc.start_auto_message_fetching(),
        "send_msg" => drop(mc.send_msg(key, "hi", Some(T), 0).await),
        "send_msg_attempt" => drop(mc.send_msg(key, "hi", Some(T), 2).await),
        "send_cmd_repeater" => {
            drop(
                mc.send_cmd(key, "ver", Some(T), Some(ContactType::Repeater))
                    .await,
            );
        }
        "send_cmd_chat" => drop(
            mc.send_cmd(key, "ver", Some(T), Some(ContactType::Chat))
                .await,
        ),
        "send_cmd_contact" => drop(mc.send_cmd(&cached(&chat), "ver", Some(T), None).await),
        "send_msg_with_retry" => drop(mc.send_msg_with_retry(key, "hi", retry).await),
        "send_msg_with_retry_prefix" => {
            let prefix: [u8; 6] = KEY[..6].try_into().unwrap();
            drop(mc.send_msg_with_retry(prefix, "hi", retry).await);
        }
        "send_chan_msg" => drop(mc.send_chan_msg(0, "hi", Some(T)).await),
        "login" => drop(mc.send_login(&key, "pw").await),
        "login_sync" => drop(mc.send_login_sync(&key, "pw", options).await),
        "logout" => drop(mc.send_logout(&key).await),
        "status_req" => drop(mc.send_statusreq(&key).await),
        "telemetry_req" => drop(mc.send_telemetry_req(&key).await),
        "path_discovery" => drop(mc.send_path_discovery(&key).await),
        "path_discovery_sync" => drop(mc.send_path_discovery_sync(&key, options).await),
        "trace" => drop(mc.send_trace(&trace(5, None, &[0xaa, 0xbb])).await),
        "trace_flags" => {
            let request = TraceRequest {
                auth_code: 7,
                ..trace(6, Some(1), &[0xaa, 0xbb, 0xcc, 0xdd])
            };
            drop(mc.send_trace(&request).await);
        }
        "trace_text" | "trace_text_two_byte" => {
            let (tag, text) = if case == "trace_text" {
                (7, "23,5f")
            } else {
                (8, "2301,5f02")
            };
            let (path, flags) = meshcore::parse_trace_path(text).unwrap();
            drop(mc.send_trace(&trace(tag, Some(flags), &path)).await);
        }
        "raw_data" => drop(mc.send_raw_data(&[1, 2, 3, 4], &[0xaa], 0).await),
        "raw_data_zero_hop" => drop(mc.send_raw_data(&[1, 2, 3, 4], &[], 0).await),
        "channel_data" => drop(
            mc.send_channel_data(1, 0x1234, &[0xaa, 0xbb], None, 0)
                .await,
        ),
        "channel_data_path" => {
            drop(
                mc.send_channel_data(1, 0x1234, &[0xaa, 0xbb], Some(&[1, 2]), 0)
                    .await,
            );
        }
        "channel_data_two_byte" => {
            let path = [1, 2, 3, 4];
            drop(
                mc.send_channel_data(1, 0x1234, &[0xaa, 0xbb], Some(&path), 1)
                    .await,
            );
        }
        "flood_scope_name" => drop(mc.set_flood_scope_str("hamburg").await),
        "flood_scope_hashtag" => {
            drop(
                mc.set_flood_scope(&FloodScope::Name("#hamburg".into()))
                    .await,
            );
        }
        "flood_scope_unscoped" => drop(mc.set_flood_scope_str("*").await),
        "flood_scope_none" => drop(mc.set_flood_scope(&FloodScope::Default).await),
        "flood_scope_zero" => drop(mc.set_flood_scope_str("0").await),
        "flood_scope_key" => drop(mc.set_flood_scope(&FloodScope::Key(sixteen)).await),
        "force_unscoped" => drop(mc.force_unscoped().await),
        "reset_flood_scope" => drop(mc.reset_flood_scope().await),
        "default_flood_scope" => drop(mc.set_default_flood_scope(Some("hamburg")).await),
        "reset_default_flood_scope" => drop(mc.reset_default_flood_scope().await),
        "get_default_flood_scope" => drop(mc.get_default_flood_scope().await),
        "control_data" => drop(mc.send_control_data(0x90, &[1, 2]).await),
        "node_discover" => drop(mc.send_node_discover_req(0x04, true, Some(9), None).await),
        "node_discover_since" => {
            drop(
                mc.send_node_discover_req(0x1c, false, Some(10), Some(T))
                    .await,
            );
        }
        // remote nodes
        "status_sync" => drop(mc.req_status_sync(&key, options).await),
        "telemetry_sync" => drop(mc.req_telemetry_sync(&key, options).await),
        "mma_sync" => drop(mc.req_mma_sync(&key, 3600, 0, options).await),
        "acl_sync" => drop(mc.req_acl_sync(&key, options).await),
        "neighbours_sync" => {
            drop(
                mc.req_neighbours_sync(&key, NeighboursQuery::default(), options)
                    .await,
            );
        }
        "neighbours_query" => {
            let query = NeighboursQuery {
                count: 20,
                offset: 300,
                order_by: 2,
                pubkey_prefix_length: 6,
            };
            drop(mc.req_neighbours_sync(&key, query, options).await);
        }
        "neighbours_async" => {
            drop(
                mc.req_neighbours_async(&key, NeighboursQuery::default(), options)
                    .await,
            );
        }
        "fetch_all_neighbours" => drop(mc.fetch_all_neighbours(&key, 0, 4, options).await),
        "owner_info_sync" => drop(mc.req_owner_info_sync(&key, options).await),
        "subscribe_sync" => {
            let deltas = [1, 103, 10];
            drop(
                mc.req_subscribe_sync(&key, [1, 2, 3, 4], 3600, &deltas, options)
                    .await,
            );
        }
        "unsubscribe_sync" => drop(mc.req_unsubscribe_sync(&key, options).await),
        "keep_alive" => {
            drop(
                mc.send_binary_req(&key, BinaryReqType::KeepAlive, &[7], options)
                    .await,
            );
        }
        "anon_owner_flood_contact" => drop(mc.req_owner_sync(&chat, options).await),
        // the latter two with a device that leaves the hash mode query or the
        // zero-hop change unanswered
        "anon_regions_flood_contact" | "anon_hash_mode_unanswered" | "anon_zero_hop_unanswered" => {
            drop(mc.req_regions_async(&chat, options).await);
        }
        "anon_basic_flood_contact" => drop(mc.req_basic_sync(&chat, options).await),
        // where this library differs
        "set_radio_rounding" => drop(mc.set_radio(869.4625, 62.5, 9, 8, None).await),
        "set_radio_rounding_512" => drop(mc.set_radio(512.002, 62.5, 9, 8, None).await),
        "set_radio_rounding_bw" => drop(mc.set_radio(433.875, 8.001, 9, 8, None).await),
        "set_tx_power_negative" => drop(mc.set_tx_power(-9).await),
        "set_name_long" => drop(mc.set_name(&"n".repeat(40)).await),
        "set_channel_long_name" => {
            drop(
                mc.set_channel(1, &format!("#{}", "a".repeat(40)), None)
                    .await,
            );
        }
        "add_contact_long_name" => drop(mc.add_contact(&new_contact(&"n".repeat(40))).await),
        "trace_empty" => drop(mc.send_trace(&trace(8, None, &[])).await),
        "trace_not_whole_hops" => {
            drop(mc.send_trace(&trace(9, Some(1), &[0xaa, 0xbb, 0xcc])).await);
        }
        "trace_too_many_hops" => drop(mc.send_trace(&trace(10, None, &[0xaa; 65])).await),
        "raw_data_two_byte_hashes" => {
            drop(
                mc.send_raw_data(&[1, 2, 3, 4], &[0xa1, 0xa2, 0xb1, 0xb2], 1)
                    .await,
            );
        }
        "change_path_not_whole_hops" => {
            let path = [0xa1, 0xb2, 0xc3];
            drop(mc.change_contact_path(&cached(&key), &path, Some(1)).await);
        }
        "change_path_too_many_hops" => {
            drop(
                mc.change_contact_path(&cached(&key), &[7; 64], Some(0))
                    .await,
            );
        }
        "change_path_too_long" => {
            drop(
                mc.change_contact_path(&cached(&key), &[7; 66], Some(1))
                    .await,
            );
        }
        "change_path_mode_3" | "change_path_mode_4" => {
            let (path, mode): (&[u8], u8) = if case == "change_path_mode_3" {
                (&[0xa1, 0xb2, 0xc3, 0xd4], 3)
            } else {
                (&[0xa1, 0xb2, 0xc3, 0xd4, 0xe5], 4)
            };
            drop(
                mc.change_contact_path(&cached(&key), path, Some(mode))
                    .await,
            );
        }
        "channel_data_not_whole_hops" => {
            drop(
                mc.send_channel_data(1, 0x1234, &[0xaa], Some(&[1, 2, 3]), 1)
                    .await,
            );
        }
        "channel_data_too_many_hops" => {
            drop(
                mc.send_channel_data(1, 0x1234, &[0xaa], Some(&[7; 64]), 0)
                    .await,
            );
        }
        "channel_data_mode_3" | "channel_data_mode_4" => {
            let (path, mode): (&[u8], u8) = if case == "channel_data_mode_3" {
                (&[1, 2, 3, 4], 3)
            } else {
                (&[1, 2, 3, 4, 5], 4)
            };
            drop(
                mc.send_channel_data(1, 0x1234, &[0xaa], Some(path), mode)
                    .await,
            );
        }
        "raw_data_too_many_hops" => drop(mc.send_raw_data(&[1, 2, 3, 4], &[7; 64], 0).await),
        "update_contact_path_mismatch"
        | "update_contact_flood_with_path"
        | "update_contact_path_too_long" => {
            let (path_len, path) = match case {
                "update_contact_path_mismatch" => (PathLen::new(2, 0).unwrap(), vec![1]),
                "update_contact_flood_with_path" => (PathLen::NONE, vec![1]),
                // 33 hops of 2 bytes
                _ => (PathLen::from_byte(0x40 | 0x21), vec![1; 66]),
            };
            let mut contact = cached(&key);
            contact.out_path_len = path_len;
            contact.out_path = path;
            drop(mc.update_contact(&contact, None).await);
        }
        "send_cmd_bare_key" => drop(mc.send_cmd(chat, "ver", Some(T), None).await),
        "retry_unknown_prefix" => {
            let prefix: [u8; 6] = STRANGER[..6].try_into().unwrap();
            drop(mc.send_msg_with_retry(prefix, "hi", retry).await);
        }
        "anon_unknown_node" => {
            drop(
                mc.req_regions_async(&PublicKey::new(STRANGER), options)
                    .await,
            );
        }
        "anon_contact_with_path" => drop(mc.req_regions_async(&key, options).await),
        "devicepin_illegal" => drop(mc.set_devicepin(12_345).await),
        "coords_out_of_range" => drop(mc.set_coords(91.0, 0.0).await),
        "radio_out_of_range" => drop(mc.set_radio(433.875, 62.5, 4, 8, None).await),
        "radio_negative_frequency" => drop(mc.set_radio(-1.0, 62.5, 9, 8, None).await),
        "radio_negative_bandwidth" => drop(mc.set_radio(433.875, -62.5, 9, 8, None).await),
        "tx_power_above_max" => drop(mc.set_tx_power(23).await),
        "tx_power_below_min" => drop(mc.set_tx_power(-10).await),
        "path_hash_mode_reserved" => drop(mc.set_path_hash_mode(3).await),
        "custom_var_without_name" => drop(mc.set_custom_var("", "1").await),
        "custom_var_colon" => drop(mc.set_custom_var("a:b", "c").await),
        "custom_var_too_short" => drop(mc.set_custom_var("a", "").await),
        "command_too_long" => drop(mc.import_contact(&[0xaa; 176]).await),
        "empty_message" => drop(mc.send_msg(key, "", Some(T), 0).await),
        "message_too_long" => drop(mc.send_msg(key, &"x".repeat(161), Some(T), 0).await),
        "fifth_attempt_too_long" => drop(mc.send_msg(key, &"x".repeat(159), Some(T), 4).await),
        "empty_node_command" | "node_command_too_long" | "node_command_nul" => {
            let command = match case {
                "empty_node_command" => String::new(),
                "node_command_too_long" => "x".repeat(161),
                _ => "v\0r".to_string(),
            };
            let repeater = Some(ContactType::Repeater);
            drop(mc.send_cmd(key, &command, Some(T), repeater).await);
        }
        "channel_message_too_long" => drop(mc.send_chan_msg(0, &"x".repeat(155), Some(T)).await),
        "empty_name" => drop(mc.set_name("").await),
        "short_cli_command" => drop(mc.run_cli_command("v").await),
        "empty_sign_chunk" => drop(mc.sign_data(&[]).await),
        "short_raw_packet" => drop(mc.send_raw_packet(&[0x11]).await),
        "short_contact_card" => drop(mc.import_contact(&[0xaa; 97]).await),
        "control_type_without_top_bit" => drop(mc.send_control_data(0x10, &[1]).await),
        "long_password" => drop(mc.send_login(&key, &"p".repeat(16)).await),
        "message_nul" => drop(mc.send_msg(key, "a\0b", Some(T), 0).await),
        "channel_message_nul" => drop(mc.send_chan_msg(0, "a\0b", Some(T)).await),
        "cli_command_nul" => drop(mc.run_cli_command("ve\0r").await),
        "name_nul" => drop(mc.set_name("a\0b").await),
        "contact_name_nul" => {
            // set as a name; Contact::new would cut it at the NUL
            let mut contact = new_contact("");
            contact.name = "a\0b".into();
            drop(mc.add_contact(&contact).await);
        }
        "channel_name_nul" => drop(mc.set_channel(1, "#a\0b", None).await),
        "custom_var_nul" => drop(mc.set_custom_var("gps", "1\0").await),
        "default_flood_scope_nul" => drop(mc.set_default_flood_scope(Some("a\0b")).await),
        "password_nul" => drop(mc.send_login(&key, "pa\0ss").await),
        other => panic!("no calls for the case {other}"),
    }
}

/// The command codes a case has the device refuse (with an error reply) and
/// leave unanswered.
#[derive(Default)]
struct Answers {
    refuse: Vec<u8>,
    silent: Vec<u8>,
}

/// What a case writes: its calls on a client that has connected and, when
/// the case says so, read the contacts. The device answers them as `answers`
/// says.
async fn run(case: &str, read_contacts: bool, answers: Answers) -> Vec<Vec<u8>> {
    let changed = Arc::new(Mutex::new(Answers::default()));
    let responder = {
        let changed = Arc::clone(&changed);
        move |cmd: &[u8]| {
            let changed = changed.lock().unwrap();
            if changed.silent.contains(&cmd[0]) {
                Vec::new()
            } else if changed.refuse.contains(&cmd[0]) {
                // ERR_CODE_UNSUPPORTED_CMD
                vec![vec![1, 1]]
            } else {
                respond(cmd)
            }
        }
    };
    let (mc, device) = client_on_device(Options::default(), responder);
    mc.connect().await.unwrap();
    if read_contacts {
        mc.get_contacts(0).await.unwrap();
    }
    // from the case's own calls on
    *changed.lock().unwrap() = answers;
    let start = device.commands().len();
    act(case, &mc).await;
    device.settle().await;
    device.commands().split_off(start)
}

/// The frames as hex, with the bytes chosen at random (the nonce of a
/// neighbours request) set to zero.
fn comparable(frames: &[Vec<u8>]) -> Vec<String> {
    frames
        .iter()
        .map(|frame| {
            let mut frame = frame.clone();
            if frame.len() == 44 && frame[0] == 50 && frame[33] == 6 {
                frame[40..].fill(0);
            }
            meshcore::hex::encode(frame)
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn commands_go_out_as_meshcore_py_sends_them() {
    let fixture = include_str!("fixtures/meshcore_py_commands.jsonl");
    let mut cases = Vec::new();
    let mut mismatches = Vec::new();
    for line in fixture.lines() {
        let recorded: serde_json::Value = serde_json::from_str(line).unwrap();
        let case = recorded["case"].as_str().unwrap().to_string();
        let py: Vec<Vec<u8>> = recorded["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|frame| meshcore::hex::decode(frame.as_str().unwrap()).unwrap())
            .collect();
        let py = comparable(&py);
        let codes = |kind: &str| -> Vec<u8> {
            let codes = recorded[kind].as_array().map_or(&[][..], Vec::as_slice);
            let code = |code: &serde_json::Value| u8::try_from(code.as_u64().unwrap()).unwrap();
            codes.iter().map(code).collect()
        };
        let answers = Answers {
            refuse: codes("refuse"),
            silent: codes("silent"),
        };
        let rs = comparable(&run(&case, recorded["contacts"] == true, answers).await);
        let raised = recorded["raised"].as_str();
        let listed = RAISED.iter().find(|(name, _)| *name == case);
        if raised != listed.map(|(_, error)| *error) {
            mismatches.push(format!("{case}: meshcore_py raises {raised:?}"));
        }
        match DEVIATIONS.iter().find(|(name, ..)| *name == case) {
            Some((_, py_expected, rs_expected)) => {
                if py != *py_expected || rs != *rs_expected {
                    mismatches.push(format!(
                        "{case} (deviation)\n  meshcore_py: {py:?}\n  meshcore-rs: {rs:?}"
                    ));
                }
            }
            None if py != rs => mismatches.push(format!(
                "{case}\n  meshcore_py: {py:?}\n  meshcore-rs: {rs:?}"
            )),
            None => {}
        }
        cases.push(case);
    }
    // each case listed is one meshcore_py has
    let listed = DEVIATIONS.iter().map(|(case, ..)| case);
    for case in listed.chain(RAISED.iter().map(|(case, _)| case)) {
        assert!(cases.iter().any(|c| c == case), "{case} is not recorded");
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
