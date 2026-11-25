"""Feeds device frames through meshcore_py's MessageReader and prints one JSON
line per event, the reference the parity test (tests/parity.rs) compares with.

Regenerate the fixtures after updating meshcore_py:

    python3 -I make_frames.py > frames.hex
    python3 meshcore_py_reader.py <meshcore_py>/src <pycayennelpp source> frames.hex > meshcore_py.jsonl

meshcore_py needs pycryptodome, which python3 has to find (installed or on
PYTHONPATH, which -I would leave out), and pycayennelpp, whose source the
second argument gives; its serial module is stubbed (it does without bleak),
nothing is opened. RX log receive times are dropped (they are the time of the
run).
"""
import asyncio
import json
import sys
import types

meshcore_src, cayennelpp_src, frames_path = sys.argv[1:4]
sys.path.insert(0, cayennelpp_src)
sys.path.insert(0, meshcore_src)

# meshcore/__init__.py imports the serial transport; it is never used here.
stub = types.ModuleType("serial_asyncio_fast")
stub.SerialTransport = type("SerialTransport", (), {})
sys.modules["serial_asyncio_fast"] = stub

from meshcore.reader import MessageReader  # noqa: E402


class Collect:
    def __init__(self):
        self.events = []

    async def dispatch(self, event):
        self.events.append(event)


def default(value):
    if isinstance(value, (bytes, bytearray)):
        return value.hex()
    raise TypeError(repr(value))


def dumps(value):
    return json.dumps(value, default=default, separators=(",", ":"), ensure_ascii=False)


async def main():
    collect = Collect()
    reader = MessageReader(collect)
    for line in open(frames_path):
        collect.events.clear()
        await reader.handle_rx(bytearray.fromhex(line.strip()))
        for event in collect.events:
            payload = event.payload
            if isinstance(payload, dict):
                payload = dict(payload)
                payload.pop("recv_time", None)
            print(dumps({"type": event.type.value, "payload": payload}))
        if not collect.events:
            print(dumps({"type": None}))


asyncio.run(main())
