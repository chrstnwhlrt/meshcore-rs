"""Builds device frames covering every packet type of the companion protocol
(in the layouts of firmware v1.18, some cut short or with content the
firmware does not send) and prints them as hex, one per line. The
parity test feeds them to the client and compares the events with what
meshcore_py makes of the same frames (see meshcore_py_reader.py). It needs
pycryptodome to encrypt a channel packet."""
import hashlib, hmac, struct, sys

from Crypto.Cipher import AES

def u32(v): return struct.pack('<I', v)
def i32(v): return struct.pack('<i', v)
def u16(v): return struct.pack('<H', v)
KEY = bytes([0x11]) * 32
KEY2 = bytes(range(32))

def contact(code, key, name, plen, path, lat=53551100, lon=-9993700):
    f = bytes([code]) + key + bytes([2, 1, plen]) + path.ljust(64, b'\0')
    return f + name.ljust(32, b'\0') + u32(1000) + i32(lat) + i32(lon) + u32(2000)

frames = []
add = frames.append
add(b'\x00')
add(b'\x00' + u32(1234))
# a value only in a frame of 5 bytes
add(b'\x00' + u32(1234) + b'\x00')
add(b'\x01\x06')
add(b'\x01\x2a')
si = bytes([5, 1, 0xf7, 22]) + bytes([0x22]) * 32 + i32(53551100) + i32(9993700) + bytes([0, 1, 0b100110, 1]) + u32(869525) + u32(250000) + bytes([11, 5]) + b'MyNode'
add(si)
di = bytes([13, 14, 175, 40]) + u32(123456) + b'15 Oct 2026'.ljust(12, b'\0') + b'Heltec V3'.ljust(40, b'\0') + b'v1.18.0'.ljust(20, b'\0') + bytes([1, 1])
add(di)
add(bytes([12]) + u16(3540) + u32(1024) + u32(4096))
add(bytes([12]) + u16(3540))
add(bytes([9]) + u32(1700000000))
add(bytes([2]) + u32(3))
# renamed from "Alexandra" by an advert: the firmware leaves the old name's
# tail after the new name's NUL
add(contact(3, KEY, b'Alice\0dra', 0xff, b''))
add(contact(3, KEY2, b'Rep\xc3', 0x42, bytes([0xaa, 0x00, 0xbb, 0xcc])))
add(bytes([4]) + u32(2000))
add(contact(0x8a, KEY2, b'Stranger', 0x01, bytes([0x5f])))
add(bytes([6, 1]) + u32(0x12345678) + u32(5000))
add(bytes([7]) + KEY[:6] + bytes([0xff, 0]) + u32(1700000001) + b'hello')
add(bytes([16, 0xf9, 0, 0]) + KEY[:6] + bytes([0x43, 2]) + u32(1700000002) + b'SIGN' + b'signed hi')
add(bytes([8, 1, 2, 0]) + u32(1700000003) + b'bob: chan v1\0\0')
add(bytes([17, 0x16, 0, 0, 3, 0xff, 0]) + u32(1700000004) + b'carol: chan v3')
add(bytes([27, 0x08, 0, 0, 3, 0x41]) + u16(0x1234) + bytes([2, 0xaa, 0xbb]))
add(b'\x0a')
add(b'\x83')
add(bytes([11]) + bytes(range(10)))
add(bytes([21]) + b'gps:1,bad,url:http://x,gps:0')
add(bytes([24, 0]) + u16(4100) + u32(3600) + u16(5) + bytes([3]))
add(bytes([24, 1]) + struct.pack('<h', -110) + struct.pack('<b', -80) + struct.pack('<b', 41) + u32(100) + u32(200))
add(bytes([24, 2]) + u32(10) + u32(20) + u32(3) + u32(4) + u32(5) + u32(6) + u32(7))
add(bytes([24, 2]) + u32(10) + u32(20) + u32(3) + u32(4) + u32(5) + u32(6))
add(bytes([25, 0x0e]))
add(bytes([25, 0x0e, 3]))
add(bytes([18, 2]) + b'#public'.ljust(32, b'\0') + bytes(range(16)))
add(bytes([0x80]) + KEY)
add(bytes([0x81]) + KEY2)
add(bytes([0x82]) + u32(0x12345678) + u32(321))
add(bytes([0x82]) + u32(0x12345678))
add(bytes([0x84, 0x14, 0xb0, 0xff]) + bytes([1, 2, 3, 4, 5]))
add(bytes([0x85, 1]) + KEY[:6] + u32(99) + bytes([3, 2]))
add(bytes([0x85, 0]) + KEY[:6])
add(bytes([0x86, 0]) + KEY[:6])
add(bytes([0x87, 0]) + KEY[:6] + u16(4100) + bytes(50) + u32(7))
adv = KEY2 + u32(1000) + bytes(64) + bytes([0x92]) + i32(53551100) + i32(9993700) + b'Rep1'
add(bytes([0x88, 0x18, 0xa6]) + bytes([0x11, 0x01, 0x5f]) + adv)
add(bytes([0x88, 0x18, 0xa6]) + bytes([0x15, 0x00, 0x99, 0x01, 0x02]) + bytes(16))
add(bytes([0x88, 0x18, 0xa6, 0x15]))
# A group text of #public (the channel info above: index 2, secret 00..0f)
# in the RX log, which decrypts it; then the device delivers the message in
# both frame versions, and they get the path and signal it was logged with.
secret = bytes(range(16))
plain = u32(1700000005) + bytes([0]) + b'dave: logged'
crypted = AES.new(secret, AES.MODE_ECB).encrypt(plain.ljust(32, b'\0'))
mac = hmac.new(secret, crypted, hashlib.sha256).digest()[:2]
group = bytes([0x15, 0x02, 0xaa, 0xbb]) + hashlib.sha256(secret).digest()[:1] + mac + crypted
add(bytes([0x88, 0x18, 0xa6]) + group)
add(bytes([8, 2, 2, 0]) + u32(1700000005) + b'dave: logged')
add(bytes([17, 0x16, 0, 0, 2, 2, 0]) + u32(1700000005) + b'dave: logged')
add(bytes([0x89, 0, 4, 1]) + u32(7) + u32(9) + bytes([0xaa, 0xab, 0xbb, 0xbc, 8, 0xfc, 12]))
# a trace of no whole hops, as another node may build it
add(bytes([0x89, 0, 5, 1]) + u32(8) + u32(9) + bytes([0xaa, 0xab, 0xbb, 0xbc, 0xcc, 8, 0xfc, 12]))
tele = bytes([1, 116, 0x01, 0x9c, 1, 103, 0xff, 0xdd, 2, 136]) + (535511).to_bytes(3, 'big', signed=True) + (-99937).to_bytes(3, 'big', signed=True) + (1234).to_bytes(3, 'big', signed=True) + bytes([3, 117, 0xfe, 0x0c, 4, 113, 0, 10, 0xff, 0xf6, 3, 0xe8, 5, 135, 255, 0, 128]) + bytes(4)
add(bytes([0x8b, 0]) + KEY[:6] + tele)
add(bytes([0x8b, 0]) + KEY[:6] + bytes([1, 116, 0x01, 0x9c, 1, 200, 1, 2]))
add(bytes([26]) + u32(433000) + u32(434000) + u32(869000) + u32(870000))
add(bytes([0x8c, 0]) + u32(0xdeadbeef) + bytes([1, 2, 3]))
# Answers to binary requests both sides track (tags 0xa1000000 + request
# type, see meshcore_py_reader.py and tests/parity.rs): status, telemetry,
# min/max/average, access list, neighbours.
def answer(kind, data): return bytes([0x8c, 0]) + u32(0xa1000000 + kind) + data
status = u16(4100) + u16(1) + struct.pack('<hh', -110, -70) + b''.join(u32(v) for v in (10, 20, 30, 40, 5, 6, 7, 8))
status += u16(1) + struct.pack('<h', 26) + u16(2) + u16(3) + u32(50) + u32(9)
add(answer(1, status))
add(answer(3, bytes([1, 116, 0x01, 0x9c, 2, 103, 0x00, 0xfa, 6, 134, 0x00, 0x64, 0xff, 0x9c, 0x01, 0xf4])))
add(answer(4, u32(1700000006) + bytes([1, 103, 0, 200, 0, 255, 0, 221, 2, 116, 1, 0x90, 1, 0x9c, 1, 0x96])))
add(answer(5, KEY[:6] + bytes([3]) + bytes(7)))
add(answer(6, struct.pack('<hh', 5, 2) + bytes([0xa0] * 4) + struct.pack('<i', 60) + bytes([20]) + bytes([0xa1] * 4) + struct.pack('<i', 90) + bytes([0xf8])))
add(bytes([0x8d, 0]) + KEY[:6] + bytes([0xff, 0x41, 0xaa, 0xbb]))
add(bytes([14]) + bytes(range(64)))
add(bytes([19, 0]) + u32(8192))
add(bytes([20]) + bytes(range(64)))
add(b'\x0f')
add(bytes([0x8e, 0x14, 0xa6, 0x00, 0x92, 0x10]) + u32(0xdeadbeef) + bytes([7]) * 8)
add(bytes([0x8f]) + KEY)
add(b'\x90')
add(bytes([23]) + u32(300) + u32(1500))
add(bytes([28]) + b'#ham\0rg'.ljust(31, b'\0') + bytes([5]) * 16)
add(b'\x1c')
add(bytes([29]) + b'> tx 22')
add(bytes([22]) + u32(1700000000) + bytes([0x42, 1, 2, 3, 4]))
for f in frames:
    print(f.hex())
