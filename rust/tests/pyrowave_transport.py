"""Read native RTP fixtures using the pinned original C++ FEC decoder.

Usage: pyrowave_transport.py FIXTURE_DIRECTORY NANORS_REFERENCE_DLL CLIENT_EXE
No host or device settings are changed. The DLL is built by build-fec-reference.ps1.
"""
import ctypes as C
import json
import pathlib
import struct
import subprocess
import sys
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

directory, reference, decoder = map(pathlib.Path, sys.argv[1:4])
library = C.CDLL(str(reference.resolve()))
library.fec_reference_init()
byte = C.c_ubyte
pointer = C.POINTER(byte)
new = C.CFUNCTYPE(C.c_void_p, C.c_int, C.c_int)(C.c_void_p.in_dll(library, "reed_solomon_new_fn").value)
release = C.CFUNCTYPE(None, C.c_void_p)(C.c_void_p.in_dll(library, "reed_solomon_release_fn").value)
recover = C.CFUNCTYPE(C.c_int, C.c_void_p, C.POINTER(pointer), pointer, C.c_int, C.c_int)(C.c_void_p.in_dll(library, "reed_solomon_decode_fn").value)
encode = library.fec_reference_encode
encode.argtypes = [C.c_int, C.c_int, C.POINTER(pointer), C.c_int]
encode.restype = C.c_int

def arrays(packets):
    buffers = [(byte * len(p)).from_buffer_copy(p) for p in packets]
    return buffers, (pointer * len(buffers))(*(C.cast(b, pointer) for b in buffers))

results = []
for fixture in sorted(directory.glob("*.rtp")):
    raw = fixture.read_bytes()
    packets, offset = [], 0
    while offset < len(raw):
        size, = struct.unpack_from("<I", raw, offset)
        offset += 4
        assert size >= 40 and offset + size <= len(raw)
        packets.append(raw[offset:offset + size])
        offset += size
    encrypted = fixture.stem.endswith(".encrypted")
    if encrypted:
        nonces = [packet[:12] for packet in packets]
        assert len(set(nonces)) == len(packets)
        packets = [AESGCM(bytes(range(16))).decrypt(packet[:12], packet[32:] + packet[16:32], None) for packet in packets]
    blocks = {}
    for number, packet in enumerate(packets):
        assert packet[0] == 0x90
        assert struct.unpack_from(">H", packet, 2)[0] == number
        assert struct.unpack_from(">I", packet, 4)[0] == 9000
        info, = struct.unpack_from("<I", packet, 28)
        block = (packet[27] >> 4) & 3
        count, index, percentage = info >> 22, (info >> 12) & 1023, (info >> 4) & 255
        group = blocks.setdefault(block, {"count": count, "rate": percentage, "packets": {}})
        assert (group["count"], group["rate"]) == (count, percentage)
        assert index not in group["packets"]
        group["packets"][index] = packet
    assert len(blocks) == (packets[0][27] >> 6) + 1 <= 4
    payloads, recovered = [], 0
    for block, group in sorted(blocks.items()):
        count = group["count"]
        ordered = [group["packets"][i] for i in range(len(group["packets"]))]
        parity = len(ordered) - count
        assert count <= 1023 and (not parity or count + parity <= 255)
        if parity:
            expected, pointers = arrays(ordered[:count] + [bytes(len(ordered[0]))] * parity)
            assert encode(count, parity, pointers, len(ordered[0])) == 0
            for i in range(parity):
                assert bytes(expected[count + i])[32:] == ordered[count + i][32:]
            # Lose the first coarse shards, including the short/sequence headers.
            missing = min(parity, count, 2)
            surviving, pointers = arrays(ordered)
            marks = (byte * len(ordered))()
            for i in range(missing):
                marks[i] = 1
                C.memset(surviving[i], 0, len(ordered[i]))
            matrix = new(count, parity)
            assert matrix
            try:
                assert recover(matrix, pointers, marks, len(ordered), len(ordered[0])) == 0
            finally:
                release(matrix)
            for i in range(missing):
                assert bytes(surviving[i])[32:] == ordered[i][32:]
                assert surviving[i][26] == ordered[i][26]  # record restart flag is protected
            recovered += missing
            ordered = [bytes(b) for b in surviving]
        payloads.extend(p[32:] for p in ordered[:count])
    joined = b"".join(payloads)
    last, critical = struct.unpack_from("<HH", joined, 4)
    assert joined[3] == 2
    total = (len(payloads) - 1) * len(payloads[0]) + last
    frame = joined[8:total]
    profile = fixture.stem.removesuffix(".encrypted")
    original = (directory / (profile + ".bin")).read_bytes()
    assert frame == original
    records = fixture.name.startswith("records-")
    assert bool(critical) == records
    assert (packets[0][26] & 0x80 != 0) == records
    if records:
        assert recovered > 0 and blocks[0]["rate"] >= 20
    else:
        assert recovered == 0
    restored = fixture.with_suffix(".recovered.bin")
    restored.write_bytes(frame)
    output = subprocess.check_output([str(decoder.resolve()), str(restored.resolve())], text=True)
    assert "PYROWAVE DECODE PASS" in output
    results.append({"profile": profile, "encrypted": encrypted, "packets": len(packets), "blocks": len(blocks), "critical_shards": critical, "lost_coarse_shards_recovered": recovered, "parity": "original C++ byte match", "decoded": True})
assert len(results) == 24
print(json.dumps({"status": "pass", "profiles": results}, indent=2))
