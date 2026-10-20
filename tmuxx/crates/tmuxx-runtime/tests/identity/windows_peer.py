"""A real native pipe client process, with a bounded parent and exact JSON I/O."""
import ctypes
import json
import msvcrt
import os
import sys

kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_ulong, ctypes.c_ulong,
                              ctypes.c_void_p, ctypes.c_ulong, ctypes.c_ulong,
                              ctypes.c_void_p]
kernel.CreateFileW.restype = ctypes.c_void_p
handle = kernel.CreateFileW(sys.argv[1], 0xC0000000, 0, None, 3, 0x110000, None)
assert handle != ctypes.c_void_p(-1).value, ctypes.get_last_error()
descriptor = msvcrt.open_osfhandle(handle, os.O_RDWR | os.O_BINARY)
with os.fdopen(descriptor, 'r+b', buffering=0) as pipe:
    reader = __import__('io').BufferedReader(pipe)
    hello = dict(version=1, cwd=os.getcwd(), environment={
        'USER': 'forged-user-must-not-be-used', 'TERM': 'tmuxx-identity-test'},
        control_commands=True, snapshot_stream=False, client_features=(1 << 4) | (1 << 18))
    pipe.write(json.dumps(hello).encode() + b'\n')
    hello = json.loads(reader.readline())
    assert hello['version'] == 1 and hello['instance_id'] and hello['client_features'] == 0
    for raw in sys.stdin.buffer:
        request = json.loads(raw)
        pipe.write(raw)
        result = bytearray()
        while True:
            raw = reader.readline()
            assert raw, 'native owner EOF before batch completion'
            record = json.loads(raw)
            if record['type'] == 'control':
                result.extend(record['data'])
                result.append(10)
            elif record['type'] == 'reply':
                assert record['request_id'] == request['request']['request_id']
                assert record['error'] is None, record
                break
        print(json.dumps(list(result)), flush=True)
