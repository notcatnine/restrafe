"""Writes THIRD_PARTY_NOTICES.txt for a binary release of Restrafe: the license texts of every crate compiled into
restrafe.exe (normal dependencies of the target platform, from `cargo metadata`) and of the vendored C libraries.
Identical texts are written once, with the crates that use them listed above.

    python scripts/third_party_notices.py <output file>      (run from the repository root)
"""
import hashlib, json, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GAME = os.path.join(ROOT, 'game')
TARGET = 'x86_64-pc-windows-msvc'
LICENSE_NAMES = ('license', 'licence', 'copying', 'notice', 'copyright', 'unlicense')

meta = json.loads(subprocess.check_output(['cargo', 'metadata', '--format-version', '1', '--filter-platform', TARGET], cwd=GAME))
packages = {p['id']: p for p in meta['packages']}
nodes = {n['id']: n for n in meta['resolve']['nodes']}
root_id = meta['resolve']['root']

# crates linked into the binary: follow normal (non-build, non-dev) dependencies from the root
used, stack = set(), [root_id]
while stack:
    pid = stack.pop()
    if pid in used:
        continue
    used.add(pid)
    for d in nodes[pid]['deps']:
        if any(k.get('kind') is None for k in d['dep_kinds']):
            stack.append(d['pkg'])
used.discard(root_id)

# standard texts for crates that declare a license without shipping its file
std_texts = {
    'MIT': open(os.path.join(GAME, 'vendor', 'bevy_pbr', 'LICENSE-MIT'), encoding='utf-8').read(),
    'Apache-2.0': open(os.path.join(GAME, 'vendor', 'bevy_pbr', 'LICENSE-APACHE'), encoding='utf-8').read(),
}

groups = {}  # text hash -> (text, [crate lines])


def add(text, who):
    text = text.strip().replace('\r\n', '\n')
    h = hashlib.sha1(text.encode()).hexdigest()
    groups.setdefault(h, (text, []))[1].append(who)


for pid in sorted(used, key=lambda i: (packages[i]['name'], packages[i]['version'])):
    p = packages[pid]
    who = f"{p['name']} {p['version']} ({p.get('license') or 'see text'})"
    folder = os.path.dirname(p['manifest_path'])
    files = sorted(f for f in os.listdir(folder) if f.lower().startswith(LICENSE_NAMES) and os.path.isfile(os.path.join(folder, f)))
    if files:
        for f in files:
            add(open(os.path.join(folder, f), encoding='utf-8', errors='replace').read(), who)
        continue
    spdx = p.get('license') or ''
    found = False
    for key, text in std_texts.items():
        if key in spdx:
            add(text, who + ' [standard text]')
            found = True
    if not found:
        add(f'License: {spdx or "not stated"} (no license file in the published crate)', who)

for lib in ('libvorbis', 'libogg'):
    add(open(os.path.join(GAME, 'vendor', lib, 'COPYING'), encoding='utf-8', errors='replace').read(), f'{lib} (vendored, built into restrafe.exe)')

out = [
    'Restrafe third-party notices',
    '',
    'Restrafe itself is released under the MIT License (LICENSE.txt). It includes the following third-party software,',
    'whose licenses are reproduced below. STRAFTAT and its content are not part of Restrafe and are not covered here.',
    '',
]
for text, who in sorted(groups.values(), key=lambda g: g[1][0].lower()):
    out.append('=' * 100)
    out.append('Used by: ' + ', '.join(who))
    out.append('-' * 100)
    out.append(text)
    out.append('')
open(sys.argv[1], 'w', encoding='utf-8', newline='\r\n').write('\n'.join(out))
print(f'{len(used)} crates, {len(groups)} distinct license texts -> {sys.argv[1]}')
