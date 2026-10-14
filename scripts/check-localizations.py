#!/usr/bin/env python3
"""Check native UI catalogs: complete keys, unique entries and template parity."""
from pathlib import Path
import json,re,subprocess
ROOT=Path(__file__).resolve().parents[1]
source=json.loads((ROOT/'macos/Resources/localization-source.json').read_text())
keys={item['key'] for item in source}
for locale in ['en','zh-Hans','ja']:
 path=ROOT/'macos/Resources'/f'{locale}.lproj/Localizable.strings'
 data=json.loads(subprocess.check_output(['plutil','-convert','json','-o','-',str(path)]))
 raw=path.read_text()
 entries=re.findall(r'^\s*"((?:[^"\\]|\\.)*)"\s*=',raw,re.M)
 assert len(entries)==len(set(entries)),(locale,'duplicate keys')
 assert keys<=data.keys(),(locale,'missing',keys-data.keys())
 for key,value in data.items():
  assert value.strip(),(locale,key,'empty')
  assert set(re.findall(r'\{\d+\}',key))==set(re.findall(r'\{\d+\}',value)),(locale,key,'placeholder mismatch')
 # Every call site has a catalog entry, including welcome and dynamic labels.
 for path in (ROOT/'macos').glob('*.swift'):
  for raw_key in re.findall(r'\b(?:L|LF|localize)\("((?:[^"\\]|\\.)*)"',path.read_text()):
   key=json.loads('"'+raw_key+'"')
   assert key in data,(locale,path.name,'missing call site',key)
 print(f'{locale}: {len(data)} translated keys; call sites and placeholders OK')
