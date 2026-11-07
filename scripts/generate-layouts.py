#!/usr/bin/env python3
"""Regenerate maps from xkeyboard-config 2.41 using libxkbcommon (build tool only).
Usage: generate-layouts.py /path/to/share/X11/xkb /path/to/libxkbcommon.so
The Nix layout check supplies pinned inputs. Output contains no user data.
"""
import ctypes as c
import hashlib
import json
from pathlib import Path
import sys

root, library = Path(sys.argv[1]), sys.argv[2]
x = c.CDLL(library)
def fn(name, result, *args):
    f = getattr(x, name); f.restype = result; f.argtypes = args; return f
ptr, uint = c.c_void_p, c.c_uint
context = fn('xkb_context_new', ptr, c.c_int)(1) # no default include paths
assert context
assert fn('xkb_context_include_path_append', c.c_int, ptr, c.c_char_p)(context, str(root).encode())
# Compile the standard pc105 component selection directly, so this works from
# the pinned source tarball as well as an installation (rules/evdev is generated
# at upstream build time and is absent from the source archive).
keymap_new = fn('xkb_keymap_new_from_string', ptr, ptr, c.c_char_p, c.c_int, c.c_int)
state_new = fn('xkb_state_new', ptr, ptr)
update = fn('xkb_state_update_mask', uint, ptr, uint, uint, uint, uint, uint, uint)
get_sym = fn('xkb_state_key_get_one_sym', uint, ptr, uint)
utf32 = fn('xkb_keysym_to_utf32', uint, uint)
mod_index = fn('xkb_keymap_mod_get_index', uint, ptr, c.c_char_p)
keys = list(range(2, 14)) + list(range(16, 28)) + list(range(30, 42)) + list(range(43, 54)) + [57, 86]
for layout in ['us', 'se']:
    description = f'''xkb_keymap {{
      xkb_keycodes {{ include "evdev+aliases(qwerty)" }};
      xkb_types {{ include "complete" }};
      xkb_compatibility {{ include "complete" }};
      xkb_symbols {{ include "pc+{layout}(basic)+inet(evdev)" }};
    }};'''
    keymap = keymap_new(context, description.encode(), 1, 0); assert keymap
    state = state_new(keymap); assert state
    shift = mod_index(keymap, b'Shift'); level3 = mod_index(keymap, b'Mod5')
    table = {}
    # Prefer fewer modifiers; first encountered mapping wins.
    for modifiers in [0, 1, 2, 3]:
        mask = ((1 << shift) if modifiers & 1 else 0) | ((1 << level3) if modifiers & 2 else 0)
        update(state, mask, 0, 0, 0, 0, 0)
        for key in keys:
            cp = utf32(get_sym(state, key + 8))
            if cp >= 32 and not 127 <= cp <= 159:
                table.setdefault(cp, [key, modifiers])
    rows = [[cp, *value] for cp, value in sorted(table.items())]
    Path(f'layouts/{layout}.json').write_text(json.dumps(rows, indent=2) + '\n')
    fn('xkb_state_unref', None, ptr)(state)
    fn('xkb_keymap_unref', None, ptr)(keymap)
fn('xkb_context_unref', None, ptr)(context)
sources = ['symbols/us', 'symbols/se', 'symbols/latin', 'types/extra', 'types/level5']
Path('layouts/SOURCES.json').write_text(json.dumps({
    'xkeyboard_config_version': '2.41', 'model': 'pc105', 'variant': 'basic',
    'sources_sha256': {s: hashlib.sha256((root / s).read_bytes()).hexdigest() for s in sources},
    'scope': 'Main alphanumeric ISO keys; direct characters under Shift/AltGr; no keypad or composition'
}, indent=2) + '\n')
