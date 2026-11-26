"""Runs commands through meshcore_py's MeshCore against a stand-in device and
prints the frames meshcore_py writes, one JSON line per case: the reference
the command parity test (tests/command_parity.rs) compares with. Each case
runs on a client of its own that has connected (and, where the case says so,
read the contacts); only the frames of the case's own calls are recorded.

The stand-in answers as the one in tests/command_parity.rs does: replies at
once, answers from the mesh (binary responses, a login) a moment later, as a
device sends them. A case may have it refuse commands (with an error reply)
or leave them unanswered, which its line records for the other side. A case
that raises records the exception's name.

Regenerate after updating meshcore_py:

    python3 meshcore_py_commands.py <meshcore_py>/src <pycayennelpp source> > meshcore_py_commands.jsonl

It needs pycryptodome, which python3 has to find (installed, or on
PYTHONPATH). meshcore_py's serial module is stubbed (it does without bleak),
nothing is opened.
"""
import asyncio
import json
import logging
import random
import struct
import sys
import types

meshcore_src, cayennelpp_src = sys.argv[1:3]
sys.path.insert(0, cayennelpp_src)
sys.path.insert(0, meshcore_src)

# meshcore/__init__.py imports the serial transport; it is never used here.
stub = types.ModuleType("serial_asyncio_fast")
stub.SerialTransport = type("SerialTransport", (), {})
sys.modules["serial_asyncio_fast"] = stub

from meshcore import MeshCore  # noqa: E402
from meshcore.packets import BinaryReqType  # noqa: E402

# meshcore_py's log lines are not compared here (tests/logging.rs does that).
logging.disable(logging.CRITICAL)
# The nonces of neighbours requests are random; seeded, the file comes out the
# same each time (the test compares the frames without them).
random.seed(1)


def u16(v): return struct.pack("<H", v)
def i16(v): return struct.pack("<h", v)
def u32(v): return struct.pack("<I", v)
def i32(v): return struct.pack("<i", v)


# A repeater with a path of one 2-byte hop, a companion that floods, a node
# that is no contact, and the time the calls give.
KEY = bytes([0x11]) * 32
KEY2 = bytes([0x33]) * 32
STRANGER = bytes([0x55]) * 32
T = 1700000000

CONTACTS = {
    KEY: (2, b"Rep", 0x41, b"\xa1\xa2"),
    KEY2: (1, b"Chat", 0xFF, b""),
}


def self_info(name):
    # as self_info_frame in tests/common/mod.rs
    return (bytes([5, 1, 22, 22]) + bytes([0x22]) * 32 + i32(53551100) + i32(9993700)
            + bytes([0, 1, 0b100110, 0]) + u32(869525) + u32(250000) + bytes([11, 5]) + name)


def device_info(path_hash_mode):
    # as device_info_frame in tests/common/mod.rs
    return (bytes([13, 14, 175, 40]) + u32(0) + b"15 Oct 2026".ljust(12, b"\0")
            + b"Heltec".ljust(40, b"\0") + b"v1.18.0".ljust(20, b"\0") + bytes([0, path_hash_mode]))


def contact(key, kind, name, path_len, path):
    return (bytes([3]) + key + bytes([kind, 0, path_len]) + path.ljust(64, b"\0")
            + name.ljust(32, b"\0") + u32(1000) + i32(53551100) + i32(9993700) + u32(2000))


def sent(tag): return bytes([6, 0]) + u32(tag) + u32(100)
def binary(tag, data): return bytes([0x8C, 0]) + u32(tag) + data


def neighbours(cmd):
    # three neighbours: two in the first page, one in the next
    offset = int.from_bytes(cmd[36:38], "little")
    count = 2 if offset == 0 else 1
    data = i16(3) + i16(count)
    for n in range(count):
        data += bytes([0xA0 + n]) * cmd[39] + i32(60) + bytes([20])
    return data


ANSWERS = {
    1: lambda cmd: u16(4100) + bytes(50) + u32(7),
    2: lambda cmd: b"",
    3: lambda cmd: bytes([1, 116, 0x01, 0x9C]),
    4: lambda cmd: u32(T) + bytes([1, 103, 0, 200, 0, 255, 0, 221]),
    5: lambda cmd: KEY[:6] + bytes([3]) + bytes(7),
    6: neighbours,
    7: lambda cmd: b"v1.18.0\nRep\nOwner\0\0",
    8: lambda cmd: u16(3600) + b"\0\0#hamburg\0",
    9: lambda cmd: b"",
}

STATS = {
    0: bytes([24, 0]) + u16(4100) + u32(3600) + u16(0) + bytes([2]),
    1: bytes([24, 1]) + i16(-110) + bytes([0xB0, 26]) + u32(10) + u32(20),
    2: bytes([24, 2]) + b"".join(u32(v) for v in range(1, 8)),
}


def respond(cmd):
    """The frames the device sends for a command: at once, and later."""
    code = cmd[0]
    if code == 1:
        return [self_info(b"Node")], []
    if code == 22:
        return [device_info(1)], []
    if code == 2:
        return [sent(0xA0000000 + cmd[2])], []
    if code == 4:
        listed = [contact(key, *fields) for key, fields in CONTACTS.items()]
        return [bytes([2]) + u32(len(CONTACTS))] + listed + [bytes([4]) + u32(5000)], []
    if code == 5:
        return [bytes([9]) + u32(T)], []
    if code == 10:
        return [bytes([10])], []
    if code == 17:
        return [bytes([11, 0xAA, 0xBB])], []
    if code == 19:
        return [], []
    if code == 20:
        return [bytes([12]) + u16(3540) + u32(1024) + u32(4096)], []
    if code == 23:
        return [bytes([14]) + bytes([0xAB]) * 64], []
    if code == 26:
        return [sent(0x1A1A1A1A)], [bytes([0x85, 1]) + cmd[1:7] + u32(T) + bytes([3, 2])]
    if code in (27, 52):
        return [sent(0x1B000000 + code)], []
    if code == 30:
        key = bytes(cmd[1:33])
        return ([contact(key, *CONTACTS[key])] if key in CONTACTS else [bytes([1, 2])]), []
    if code == 31:
        return [bytes([18, cmd[1]]) + b"#chan".ljust(32, b"\0") + bytes([cmd[1]]) * 16], []
    if code == 33:
        return [bytes([19, 0]) + u32(8192)], []
    if code == 35:
        return [bytes([20]) + bytes([0x5A]) * 64], []
    if code == 36:
        return [sent(int.from_bytes(cmd[1:5], "little"))], []
    if code == 39:
        if len(cmd) == 4:
            # the device's own telemetry, with its own key prefix
            return [bytes([0x8B, 0]) + bytes([0x22]) * 6 + bytes([1, 116, 0x01, 0x9C])], []
        return [sent(0x27000000)], []
    if code == 40:
        return [bytes([21]) + b"gps:1"], []
    if code == 42:
        return [bytes([22]) + u32(T) + bytes([0x41, 0xAA, 0xBB])], []
    if code == 43:
        return [bytes([23]) + u32(500) + u32(1200)], []
    if code == 50:
        kind = cmd[33]
        tag = 0xA1000000 + kind + ((int.from_bytes(cmd[36:38], "little") << 8) if kind == 6 else 0)
        return [sent(tag)], [binary(tag, ANSWERS[kind](cmd))]
    if code == 56:
        return [STATS[cmd[1]]], []
    if code == 57:
        tag = 0xA2000000 + cmd[33]
        text = b"Rep\nOwner\0" if cmd[33] == 2 else b"answer"
        return [sent(tag)], [binary(tag, u32(T) + text)]
    if code == 59:
        return [bytes([25, 0x0E, 3])], []
    if code == 60:
        return [bytes([26]) + u32(433000) + u32(434000)], []
    if code == 64:
        return [bytes([28]) + b"#hamburg".ljust(31, b"\0") + bytes([5]) * 16], []
    if code == 66:
        return [bytes([29]) + b"> v1.18.0"], []
    return [bytes([0])], []


class Device:
    """The stand-in: records what is written and answers as respond() says,
    but for the command codes it refuses or leaves unanswered."""

    def __init__(self):
        self.reader = None
        self.written = []
        self.refuse = []
        self.silent = []

    async def connect(self):
        return "stand-in"

    async def disconnect(self):
        pass

    def set_reader(self, reader):
        self.reader = reader

    def set_disconnect_callback(self, callback):
        pass

    async def send(self, data):
        data = bytes(data)
        self.written.append(data)
        if data[0] in self.silent:
            return
        # ERR_CODE_UNSUPPORTED_CMD
        now, later = ([bytes([1, 1])], []) if data[0] in self.refuse else respond(data)
        for frame in now:
            await self.reader.handle_rx(bytearray(frame))
        loop = asyncio.get_running_loop()
        for frame in later:
            loop.call_later(0.02, lambda f=frame: asyncio.ensure_future(self.reader.handle_rx(bytearray(f))))


def new_contact(name):
    """A contact dict as meshcore_py's CLI builds one to add."""
    return {
        "public_key": (bytes([0x44]) * 32).hex(), "type": 2, "flags": 0, "out_path": "",
        "out_path_len": -1, "out_path_hash_mode": -1, "adv_name": name, "last_advert": 0,
        "adv_lat": 0.0, "adv_lon": 0.0,
    }


async def single_setters(c):
    await c.set_multi_acks(2)
    await c.set_telemetry_mode_base(1)
    await c.set_telemetry_mode_loc(2)
    await c.set_telemetry_mode_env(1)
    await c.set_manual_add_contacts(True)
    await c.set_advert_loc_policy(0)


async def factory_reset(c):
    await c.confirm_factory_reset(await c.request_factory_reset())


OTHER_PARAMS = {
    "manual_add_contacts": True, "telemetry_mode_base": 2, "telemetry_mode_loc": 1,
    "telemetry_mode_env": 0, "adv_loc_policy": 1, "multi_acks": 3,
}

# (name, whether the contacts are read first, the calls[, the command codes the
# device refuses or leaves unanswered])
CASES = [
    # the device
    ("appstart", False, lambda mc, c: c.send_appstart()),
    ("device_query", False, lambda mc, c: c.send_device_query()),
    ("get_bat", False, lambda mc, c: c.get_bat()),
    ("get_time", False, lambda mc, c: c.get_time()),
    ("set_time", False, lambda mc, c: c.set_time(T)),
    ("reboot", False, lambda mc, c: c.reboot()),
    ("advert", False, lambda mc, c: c.send_advert()),
    ("advert_flood", False, lambda mc, c: c.send_advert(flood=True)),
    ("set_name", False, lambda mc, c: c.set_name("Node 2")),
    ("set_coords", False, lambda mc, c: c.set_coords(53.5511, 9.9937)),
    ("set_coords_south_west", False, lambda mc, c: c.set_coords(-33.8688, -151.2093)),
    ("set_tx_power", False, lambda mc, c: c.set_tx_power(20)),
    ("set_radio", False, lambda mc, c: c.set_radio(433.875, 62.5, 9, 8)),
    ("set_radio_repeat", False, lambda mc, c: c.set_radio(433.875, 62.5, 9, 8, repeat=True)),
    ("set_tuning", False, lambda mc, c: c.set_tuning(500, 1200)),
    ("get_tuning", False, lambda mc, c: c.get_tuning()),
    ("set_other_params", False, lambda mc, c: c.set_other_params_from_infos(dict(OTHER_PARAMS))),
    ("single_setters", False, lambda mc, c: single_setters(c)),
    ("set_devicepin", False, lambda mc, c: c.set_devicepin(123456)),
    ("self_telemetry", False, lambda mc, c: c.get_self_telemetry()),
    ("custom_vars", False, lambda mc, c: c.get_custom_vars()),
    ("set_custom_var", False, lambda mc, c: c.set_custom_var("gps", "1")),
    ("get_channel", False, lambda mc, c: c.get_channel(2)),
    ("set_hashtag_channel", False, lambda mc, c: c.set_channel(1, "#hamburg")),
    ("set_private_channel", False, lambda mc, c: c.set_channel(2, "Private", bytes(range(16)))),
    ("export_private_key", False, lambda mc, c: c.export_private_key()),
    ("import_private_key", False, lambda mc, c: c.import_private_key(bytes([0xAB]) * 64)),
    ("sign", False, lambda mc, c: c.sign(bytes(range(250)), 120)),
    ("has_connection", False, lambda mc, c: c.has_connection(KEY)),
    ("stats_core", False, lambda mc, c: c.get_stats_core()),
    ("stats_radio", False, lambda mc, c: c.get_stats_radio()),
    ("stats_packets", False, lambda mc, c: c.get_stats_packets()),
    ("allowed_repeat_freq", False, lambda mc, c: c.get_allowed_repeat_freq()),
    ("set_path_hash_mode", False, lambda mc, c: c.set_path_hash_mode(1)),
    ("get_path_hash_mode", False, lambda mc, c: c.get_path_hash_mode()),
    ("cli_command", False, lambda mc, c: c.run_cli_command("ver")),
    ("raw_packet", False, lambda mc, c: c.send_raw_packet(bytes([0x11, 0, 9, 9]))),
    ("raw_packet_priority", False, lambda mc, c: c.send_raw_packet(bytes([0x11, 0, 9, 9]), priority=2)),
    ("factory_reset", False, lambda mc, c: factory_reset(c)),
    # contacts
    ("get_contacts", False, lambda mc, c: c.get_contacts()),
    ("get_contacts_since", False, lambda mc, c: c.get_contacts(lastmod=5000)),
    ("get_contacts_async", False, lambda mc, c: c.get_contacts_async()),
    ("ensure_contacts", False, lambda mc, c: mc.ensure_contacts()),
    ("reset_path", False, lambda mc, c: c.reset_path(KEY)),
    ("share_contact", False, lambda mc, c: c.share_contact(KEY)),
    ("export_self", False, lambda mc, c: c.export_contact()),
    ("export_contact", False, lambda mc, c: c.export_contact(KEY)),
    ("import_contact", False, lambda mc, c: c.import_contact(bytes([0xAA]) * 98)),
    ("remove_contact", False, lambda mc, c: c.remove_contact(KEY)),
    ("update_contact", True, lambda mc, c: c.update_contact(mc.contacts[KEY.hex()])),
    ("update_contact_lastmod", True, lambda mc, c: c.update_contact(mc.contacts[KEY.hex()], lastmod=7000)),
    ("add_contact", False, lambda mc, c: c.add_contact(new_contact("New"))),
    ("change_path_device_mode", True, lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "a1b2c3d4")),
    ("change_path_given_mode", True,
     lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "a1b2c3", path_hash_mode=0)),
    ("change_path_mode_in_text", True, lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "a1b2c3d4e5f6:2")),
    ("change_path_zero_hop", True, lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "", path_hash_mode=0)),
    ("change_flags", True, lambda mc, c: c.change_contact_flags(mc.contacts[KEY.hex()], 1)),
    ("autoadd", False, lambda mc, c: c.set_autoadd_config(0x02)),
    ("autoadd_hops", False, lambda mc, c: c.set_autoadd_config(0x0E, 3)),
    ("get_autoadd", False, lambda mc, c: c.get_autoadd_config()),
    ("contact_by_key", False, lambda mc, c: c.get_contact_by_key(KEY)),
    ("advert_path", False, lambda mc, c: c.get_advert_path(KEY)),
    # messages
    ("get_msg", False, lambda mc, c: c.get_msg()),
    ("auto_fetch", False, lambda mc, c: mc.start_auto_message_fetching()),
    ("send_msg", False, lambda mc, c: c.send_msg(KEY, "hi", T)),
    ("send_msg_attempt", False, lambda mc, c: c.send_msg(KEY, "hi", T, attempt=2)),
    ("send_cmd_repeater", False, lambda mc, c: c.send_cmd(KEY, "ver", T, dst_type=2)),
    ("send_cmd_chat", False, lambda mc, c: c.send_cmd(KEY, "ver", T, dst_type=1)),
    ("send_cmd_contact", True, lambda mc, c: c.send_cmd(mc.contacts[KEY2.hex()], "ver", T)),
    ("send_msg_with_retry", False, lambda mc, c: c.send_msg_with_retry(KEY, "hi", timestamp=T)),
    ("send_msg_with_retry_prefix", False, lambda mc, c: c.send_msg_with_retry(KEY[:6], "hi", timestamp=T)),
    ("send_chan_msg", False, lambda mc, c: c.send_chan_msg(0, "hi", T)),
    ("login", False, lambda mc, c: c.send_login(KEY, "pw")),
    ("login_sync", False, lambda mc, c: c.send_login_sync(KEY, "pw")),
    ("logout", False, lambda mc, c: c.send_logout(KEY)),
    ("status_req", False, lambda mc, c: c.send_statusreq(KEY)),
    ("telemetry_req", False, lambda mc, c: c.send_telemetry_req(KEY)),
    ("path_discovery", False, lambda mc, c: c.send_path_discovery(KEY)),
    ("path_discovery_sync", False, lambda mc, c: c.send_path_discovery_sync(KEY)),
    ("trace", False, lambda mc, c: c.send_trace(tag=5, path=b"\xaa\xbb")),
    ("trace_flags", False, lambda mc, c: c.send_trace(auth_code=7, tag=6, flags=1, path=b"\xaa\xbb\xcc\xdd")),
    ("trace_text", False, lambda mc, c: c.send_trace(tag=7, path="23,5f")),
    ("trace_text_two_byte", False, lambda mc, c: c.send_trace(tag=8, path="2301,5f02")),
    ("raw_data", False, lambda mc, c: c.send_raw_data(bytes([1, 2, 3, 4]), b"\xaa")),
    ("raw_data_zero_hop", False, lambda mc, c: c.send_raw_data(bytes([1, 2, 3, 4]))),
    ("channel_data", False, lambda mc, c: c.send_channel_data(1, 0x1234, b"\xaa\xbb")),
    ("channel_data_path", False, lambda mc, c: c.send_channel_data(1, 0x1234, b"\xaa\xbb", path=b"\x01\x02")),
    ("channel_data_two_byte", False,
     lambda mc, c: c.send_channel_data(1, 0x1234, b"\xaa\xbb", path=b"\x01\x02\x03\x04", path_hash_mode=1)),
    ("flood_scope_name", False, lambda mc, c: c.set_flood_scope("hamburg")),
    ("flood_scope_hashtag", False, lambda mc, c: c.set_flood_scope("#hamburg")),
    ("flood_scope_unscoped", False, lambda mc, c: c.set_flood_scope("*")),
    ("flood_scope_none", False, lambda mc, c: c.set_flood_scope(None)),
    ("flood_scope_zero", False, lambda mc, c: c.set_flood_scope("0")),
    ("flood_scope_key", False, lambda mc, c: c.set_flood_scope(bytes(range(16)))),
    ("force_unscoped", False, lambda mc, c: c.force_unscoped()),
    ("reset_flood_scope", False, lambda mc, c: c.reset_flood_scope()),
    ("default_flood_scope", False, lambda mc, c: c.set_default_flood_scope("hamburg")),
    ("reset_default_flood_scope", False, lambda mc, c: c.reset_default_flood_scope()),
    ("get_default_flood_scope", False, lambda mc, c: c.get_default_flood_scope()),
    ("control_data", False, lambda mc, c: c.send_control_data(0x90, b"\x01\x02")),
    ("node_discover", False, lambda mc, c: c.send_node_discover_req(0x04, prefix_only=True, tag=9)),
    ("node_discover_since", False,
     lambda mc, c: c.send_node_discover_req(0x1C, prefix_only=False, tag=10, since=T)),
    # remote nodes
    ("status_sync", False, lambda mc, c: c.req_status_sync(KEY)),
    ("telemetry_sync", False, lambda mc, c: c.req_telemetry_sync(KEY)),
    ("mma_sync", False, lambda mc, c: c.req_mma_sync(KEY, 3600, 0)),
    ("acl_sync", False, lambda mc, c: c.req_acl_sync(KEY)),
    ("neighbours_sync", False, lambda mc, c: c.req_neighbours_sync(KEY)),
    ("neighbours_query", False,
     lambda mc, c: c.req_neighbours_sync(KEY, count=20, offset=300, order_by=2, pubkey_prefix_length=6)),
    ("neighbours_async", False, lambda mc, c: c.req_neighbours_async(KEY)),
    ("fetch_all_neighbours", False, lambda mc, c: c.fetch_all_neighbours(KEY)),
    ("owner_info_sync", False, lambda mc, c: c.req_owner_info_sync(KEY)),
    ("subscribe_sync", False, lambda mc, c: c.req_subscribe_sync(KEY, b"\x01\x02\x03\x04", 3600, bytes([1, 103, 10]))),
    ("unsubscribe_sync", False, lambda mc, c: c.req_unsubscribe_sync(KEY)),
    ("keep_alive", False, lambda mc, c: c.send_binary_req(KEY, BinaryReqType.KEEP_ALIVE, b"\x07")),
    ("anon_owner_flood_contact", True, lambda mc, c: c.req_owner_sync(KEY2)),
    ("anon_regions_flood_contact", True, lambda mc, c: c.req_regions_async(KEY2)),
    ("anon_basic_flood_contact", True, lambda mc, c: c.req_basic_sync(KEY2)),
    ("anon_hash_mode_unanswered", True, lambda mc, c: c.req_regions_async(KEY2), {"silent": [22]}),
    # where this library differs (see DEVIATIONS in tests/command_parity.rs)
    ("set_radio_rounding", False, lambda mc, c: c.set_radio(869.4625, 62.5, 9, 8)),
    ("set_radio_rounding_512", False, lambda mc, c: c.set_radio(512.002, 62.5, 9, 8)),
    ("set_radio_rounding_bw", False, lambda mc, c: c.set_radio(433.875, 8.001, 9, 8)),
    ("set_tx_power_negative", False, lambda mc, c: c.set_tx_power(-9)),
    ("set_name_long", False, lambda mc, c: c.set_name("n" * 40)),
    ("set_channel_long_name", False, lambda mc, c: c.set_channel(1, "#" + "a" * 40)),
    ("add_contact_long_name", False, lambda mc, c: c.add_contact(new_contact("n" * 40))),
    ("trace_empty", False, lambda mc, c: c.send_trace(tag=8)),
    ("trace_not_whole_hops", False, lambda mc, c: c.send_trace(tag=9, flags=1, path=b"\xaa\xbb\xcc")),
    ("trace_too_many_hops", False, lambda mc, c: c.send_trace(tag=10, path=b"\xaa" * 65)),
    ("raw_data_two_byte_hashes", False, lambda mc, c: c.send_raw_data(bytes([1, 2, 3, 4]), b"\xa1\xa2\xb1\xb2")),
    ("change_path_not_whole_hops", True,
     lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "a1b2c3", path_hash_mode=1)),
    ("change_path_too_many_hops", True,
     lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "07" * 64, path_hash_mode=0)),
    ("change_path_too_long", True,
     lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "07" * 66, path_hash_mode=1)),
    ("change_path_mode_3", True,
     lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "a1b2c3d4", path_hash_mode=3)),
    ("change_path_mode_4", True,
     lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "a1b2c3d4e5", path_hash_mode=4)),
    ("channel_data_not_whole_hops", False,
     lambda mc, c: c.send_channel_data(1, 0x1234, b"\xaa", path=b"\x01\x02\x03", path_hash_mode=1)),
    ("channel_data_too_many_hops", False, lambda mc, c: c.send_channel_data(1, 0x1234, b"\xaa", path=b"\x07" * 64)),
    ("channel_data_mode_3", False,
     lambda mc, c: c.send_channel_data(1, 0x1234, b"\xaa", path=b"\x01\x02\x03\x04", path_hash_mode=3)),
    ("channel_data_mode_4", False,
     lambda mc, c: c.send_channel_data(1, 0x1234, b"\xaa", path=b"\x01\x02\x03\x04\x05", path_hash_mode=4)),
    ("raw_data_too_many_hops", False, lambda mc, c: c.send_raw_data(bytes([1, 2, 3, 4]), b"\x07" * 64)),
    ("update_contact_path_mismatch", True,
     lambda mc, c: c.update_contact(
         dict(mc.contacts[KEY.hex()], out_path="01", out_path_len=2, out_path_hash_mode=0))),
    ("update_contact_flood_with_path", True,
     lambda mc, c: c.update_contact(dict(mc.contacts[KEY.hex()], out_path="01", out_path_len=-1))),
    ("update_contact_path_too_long", True,
     lambda mc, c: c.update_contact(
         dict(mc.contacts[KEY.hex()], out_path="01" * 66, out_path_len=33, out_path_hash_mode=1))),
    ("send_cmd_bare_key", True, lambda mc, c: c.send_cmd(KEY2, "ver", T)),
    ("retry_unknown_prefix", False, lambda mc, c: c.send_msg_with_retry(STRANGER[:6], "hi", timestamp=T)),
    ("change_path_hash_mode_refused", True,
     lambda mc, c: c.change_contact_path(mc.contacts[KEY.hex()], "a1b2c3d4"), {"refuse": [22]}),
    ("anon_unknown_node", False, lambda mc, c: c.req_regions_async(STRANGER)),
    ("anon_contact_with_path", True, lambda mc, c: c.req_regions_async(KEY)),
    ("anon_zero_hop_unanswered", True, lambda mc, c: c.req_regions_async(KEY2), {"silent": [9]}),
    ("devicepin_illegal", False, lambda mc, c: c.set_devicepin(12345)),
    ("coords_out_of_range", False, lambda mc, c: c.set_coords(91.0, 0.0)),
    ("radio_out_of_range", False, lambda mc, c: c.set_radio(433.875, 62.5, 4, 8)),
    ("radio_negative_frequency", False, lambda mc, c: c.set_radio(-1.0, 62.5, 9, 8)),
    ("radio_negative_bandwidth", False, lambda mc, c: c.set_radio(433.875, -62.5, 9, 8)),
    ("tx_power_above_max", False, lambda mc, c: c.set_tx_power(23)),
    ("tx_power_below_min", False, lambda mc, c: c.set_tx_power(-10)),
    ("path_hash_mode_reserved", False, lambda mc, c: c.set_path_hash_mode(3)),
    ("custom_var_without_name", False, lambda mc, c: c.set_custom_var("", "1")),
    ("custom_var_colon", False, lambda mc, c: c.set_custom_var("a:b", "c")),
    ("custom_var_too_short", False, lambda mc, c: c.set_custom_var("a", "")),
    ("command_too_long", False, lambda mc, c: c.import_contact(bytes([0xAA]) * 176)),
    ("empty_message", False, lambda mc, c: c.send_msg(KEY, "", T)),
    ("message_too_long", False, lambda mc, c: c.send_msg(KEY, "x" * 161, T)),
    ("fifth_attempt_too_long", False, lambda mc, c: c.send_msg(KEY, "x" * 159, T, attempt=4)),
    ("empty_node_command", False, lambda mc, c: c.send_cmd(KEY, "", T, dst_type=2)),
    ("node_command_too_long", False, lambda mc, c: c.send_cmd(KEY, "x" * 161, T, dst_type=2)),
    ("channel_message_too_long", False, lambda mc, c: c.send_chan_msg(0, "x" * 155, T)),
    ("empty_name", False, lambda mc, c: c.set_name("")),
    ("short_cli_command", False, lambda mc, c: c.run_cli_command("v")),
    ("empty_sign_chunk", False, lambda mc, c: c.sign_data(b"")),
    ("short_raw_packet", False, lambda mc, c: c.send_raw_packet(b"\x11")),
    ("short_contact_card", False, lambda mc, c: c.import_contact(bytes([0xAA]) * 97)),
    ("control_type_without_top_bit", False, lambda mc, c: c.send_control_data(0x10, b"\x01")),
    ("long_password", False, lambda mc, c: c.send_login(KEY, "p" * 16)),
    ("message_nul", False, lambda mc, c: c.send_msg(KEY, "a\0b", T)),
    ("node_command_nul", False, lambda mc, c: c.send_cmd(KEY, "v\0r", T, dst_type=2)),
    ("channel_message_nul", False, lambda mc, c: c.send_chan_msg(0, "a\0b", T)),
    ("cli_command_nul", False, lambda mc, c: c.run_cli_command("ve\0r")),
    ("name_nul", False, lambda mc, c: c.set_name("a\0b")),
    ("contact_name_nul", False, lambda mc, c: c.add_contact(new_contact("a\0b"))),
    ("channel_name_nul", False, lambda mc, c: c.set_channel(1, "#a\0b")),
    ("custom_var_nul", False, lambda mc, c: c.set_custom_var("gps", "1\0")),
    ("default_flood_scope_nul", False, lambda mc, c: c.set_default_flood_scope("a\0b")),
    ("password_nul", False, lambda mc, c: c.send_login(KEY, "pa\0ss")),
]


async def run(name, read_contacts, calls, answers=None):
    device = Device()
    mc = MeshCore(device, default_timeout=2)
    await mc.connect()
    if read_contacts:
        await mc.commands.get_contacts()
    start = len(device.written)
    line = {"case": name}
    if read_contacts:
        line["contacts"] = True
    # from the case's own calls on
    for kind, codes in (answers or {}).items():
        setattr(device, kind, codes)
        line[kind] = codes
    try:
        await calls(mc, mc.commands)
    except Exception as err:  # noqa: BLE001 - recorded, the frames before it count
        line["raised"] = type(err).__name__
    # let the answers that come later arrive
    await asyncio.sleep(0.1)
    await mc.stop_auto_message_fetching()
    line["frames"] = [frame.hex() for frame in device.written[start:]]
    await mc.disconnect()
    return line


async def main():
    for case in CASES:
        print(json.dumps(await run(*case), separators=(",", ":")))


asyncio.run(main())
