#!/usr/bin/env python3
"""Exercise Kitty's program_status.py without its native graphics extension.
Only fast_data_types.base64_decode is substituted with Python's strict decoder.
The lifecycle, parsing, inheritance, and urgency code runs directly from Kitty.
"""
import base64
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import types

source = (Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[3] / 'kitty').resolve()
package = types.ModuleType('kitty')
package.__path__ = [str(source / 'kitty')]
sys.modules['kitty'] = package
native = types.ModuleType('kitty.fast_data_types')
native.base64_decode = lambda data, strict: base64.b64decode(data, validate=strict)
sys.modules['kitty.fast_data_types'] = native
spec = importlib.util.spec_from_file_location('kitty.program_status', source / 'kitty/program_status.py')
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
status = module.ProgramStatus()
steps = []
operations = [
    ('report', 'state=working:app=deploy:progress=0'),
    ('report', 'state=working:id=east:progress=40:title=RWFzdA'),
    ('report', 'state=blocked:id=west:kind=permission:msg=QXBwcm92ZT8'),
    ('report', 'state=working:id=west/db:app=psql:progress=100'),
    ('report', 'state=blocked:id=east:kind=question:progress=55'),
    ('report', 'state=blocked:id=west:kind=permission:msg=QXBwcm92ZT8'),
    ('report', 'state=working:app=builder'),
    ('report', 'state=done:id=west/db:title=RG9uZQ==:kind=auth:progress=10'),
    ('report', 'state=clear:id=wes'),
    ('report', 'state=clear:id=west'),
    ('report', 'state=error:id=failed:msg=RXJyb3I='),
    ('report', 'state=idle:id=idle'),
    ('exit', ''),
    ('exit', ''),
    ('report', 'state=done:id=done'),
    ('seen', ''),
    ('seen', ''),
    ('report', 'state=working:id=alive'),
    ('report', 'state=working:id=alive'),
    ('report', 'state=clear'),
]
for action, body in operations:
    if action == 'report':
        reply, changed = status.handle_escape_code(body.encode())
        assert not reply
    elif action == 'exit':
        changed = status.on_process_exit_or_prompt()
    else:
        changed = status.on_seen_by_user()
    urgent = status.most_urgent()
    steps.append(dict(action=action, body=body, changed=changed, records=status.as_list(), urgent=None if urgent is None else urgent.id))
eviction = module.ProgramStatus()
for index in range(module.MAX_RECORDS):
    eviction.handle_escape_code(f'state=working:id=item{index}'.encode())
eviction.handle_escape_code(b'state=working:id=item0')
eviction.handle_escape_code(f'state=working:id=item{module.MAX_RECORDS}'.encode())
result = dict(eviction=dict(ids=[r.id for r in eviction], urgent=eviction.most_urgent().id), kitty_commit=subprocess.run(['git', '-C', str(source), 'rev-parse', 'HEAD'], check=True, capture_output=True, text=True).stdout.strip(), substitution='fast_data_types.base64_decode uses Python base64.b64decode(validate=True)', query=module.QUERY_REPLY, steps=steps)
print(json.dumps(result, indent=2))
