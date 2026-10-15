#!/usr/bin/env node
// Split crates/ui-egui/src/canvas.rs into modules by concern (LAMB T328, upstream PR
// pr/canvas-frame, commit 1). Mechanical: every top-level item named in MANIFEST is cut from
// canvas.rs (with its leading `///`, `//` and `#[...]` lines, and the `impl` blocks of a type)
// and written to canvas/<module>.rs; nothing else changes. Run `cargo fmt` after it.
//
//   node tools/canvas-split.mjs <worktree>
//
// Regenerating the PR on a fresh upstream/main: new worktree at upstream/main, run this,
// `cargo fmt --all`, commit; then cherry-pick the hand-written show() phase split on top.
//
// Visibility: a moved private item becomes `pub(super)` (so canvas.rs and its other modules still
// see it); `pub`/`pub(crate)` are unchanged. Private fields of a moved struct and private methods
// of a moved inherent impl also become `pub(super)`, since sibling modules used to reach them.
// canvas.rs gets `mod x;` + `use self::x::*;` (`pub(crate) use` for modules whose items are
// reached from outside the canvas, e.g. `crate::canvas::draw_list`).
// Exits non-zero naming any manifest item it can't find; prints the items left in canvas.rs.

import fs from 'node:fs';
import path from 'node:path';

// module -> [doc line, items]; an item is a name, '?name' (optional: moved if present, else skipped),
// or [first, last] (every item in between, inclusive).
const MANIFEST = {
  list: [
    'The display list: building it for the view, and its primitives indexed by entity.',
    ['PrimIndex', 'prims_of', 'preview_options', 'rebuilt_preview', 'ensure_list', 'build_window'],
  ],
  viewport: [
    "The active MSPACE viewport's contents and highlights, drawn apart from the layout's list.",
    [['split_viewport', 'VpHighlight']],
  ],
  grid: ['The grid, rectangular and isometric.', ['adaptive', 'draw_grid', 'draw_iso_grid']],
  draw: [
    'Drawing display lists and highlights, with egui shapes and through the GPU.',
    [
      'color32', 'prim_color32', 'clip_infinite', 'draw_list', 'draw_highlight_gpu', 'draw_drag_gpu',
      'list_mesh_key', 'draw_list_gpu', 'Look', 'draw_highlight', 'selection_area_fill',
    ],
  ],
  cursor: [
    'The cursor: crosshair and pick box, snap markers, tracking paths, snap overrides and tooltips.',
    [
      'draw_snap_marker', 'draw_tracking', 'effective_point', 'snap_cycle', 'override_cursor', 'tooltip',
      'pickbox_half', 'draw_crosshair',
    ],
  ],
  dyn_boxes: [
    'Painting the Dynamic Input tooltip, value boxes and options menu; the input model is [`crate::dyninput`].',
    [['dynamic_input', 'dynamic_input_boxes']],
  ],
  grips: ['Clicking grips, and the menu of a multifunctional grip.', ['click_grip', 'grip_options', 'grip_menu', 'grip_at']],
  widgets: [
    'Model space tiles, the UCS icon, the viewport label, the ViewCube and the padlock sign.',
    ['tile_rects', 'tile_xf', 'draw_tiles', 'draw_lock', 'draw_ucs_icon', 'viewport_label', 'draw_viewcube'],
  ],
  pick: [
    'What a wheel notch or a click acts on: wheel zoom, the text to edit, the hatch under the cursor.',
    ['wheel_zoom_points', 'edit_text_at', 'hatch_at'],
  ],
  // Inline test modules become file modules with the same body.
  tests: [null, ['tests']],
  hatch_pick_tests: [null, ['hatch_pick_tests']],
};
// Modules whose items are reached as `crate::canvas::<item>` from outside the canvas.
const REEXPORT = new Set(['draw', 'dyn_boxes']);

const root = process.argv[2];
if (!root) {
  console.error('usage: node tools/canvas-split.mjs <worktree>');
  process.exit(2);
}
const file = path.join(root, 'crates/ui-egui/src/canvas.rs');
const dir = path.join(root, 'crates/ui-egui/src/canvas');
const lines = fs.readFileSync(file, 'utf8').replace(/\r\n/g, '\n').split('\n');

// --- Parse top-level items (rustfmt: items start and end at column 0). ---
const HEAD = /^(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|unsafe|async|extern)\s+)*(fn|struct|enum|type|const|static|mod|trait|union|use|impl|macro_rules!)\b/;
const items = []; // {name, kind, start, end, headLine}
let i = 0;
while (i < lines.length) {
  const l = lines[i];
  if (l === '' || l.startsWith('//!')) { i++; continue; }
  // Leading doc comments, comments and attributes belong to the item that follows.
  let start = i;
  while (i < lines.length && /^(\/\/|#\[|#!\[)/.test(lines[i]) && !lines[i].startsWith('//!')) {
    // A multi-line attribute: up to its closing `]` at column 0.
    if (/^#\[/.test(lines[i]) && !/\]\s*$/.test(lines[i])) {
      while (!/^\]/.test(lines[i])) i++;
    }
    i++;
  }
  if (i >= lines.length) break;
  const head = lines[i];
  const m = HEAD.exec(head);
  if (!m) throw new Error(`canvas.rs:${i + 1}: not an item start: ${head}`);
  const kind = m[1];
  let name;
  let forTrait = false;
  if (kind === 'impl') {
    const im = /^impl(?:<[^>]*>)?\s+(?:([\w:<>, ]+?)\s+for\s+)?([A-Za-z_]\w*)/.exec(head);
    name = 'impl ' + im[2];
    forTrait = !!im[1];
  } else if (kind === 'use') {
    name = 'use';
  } else {
    name = new RegExp(`\\b${kind.replace('!', '\\!')}\\s+([A-Za-z_]\\w*)`).exec(head)[1];
  }
  // Find the end: a header line (or col-0 continuation) ending in `{` opens a block closed by a
  // col-0 `}`; one ending in `;` ends the item.
  let j = i;
  let end = -1;
  for (;;) {
    const s = lines[j];
    if (j === i || /^\S/.test(s)) {
      if (/;\s*$/.test(s)) { end = j; break; }
      if (/\{\s*$/.test(s)) {
        let k = j + 1;
        while (!/^\}/.test(lines[k])) k++;
        end = k;
        break;
      }
    }
    j++;
  }
  items.push({ name, kind, start, end, head: i, forTrait });
  i = end + 1;
}

// --- Resolve the manifest. ---
const byName = new Map();
items.forEach((it, idx) => { if (it.kind !== 'impl' && it.kind !== 'use') byName.set(it.name, idx); });
const missing = [];
const moduleOf = new Map(); // item index -> module
for (const [mod, [, list]] of Object.entries(MANIFEST)) {
  for (const entry of list) {
    if (Array.isArray(entry)) {
      const [a, b] = entry;
      if (!byName.has(a)) missing.push(a);
      if (!byName.has(b)) missing.push(b);
      if (!byName.has(a) || !byName.has(b)) continue;
      for (let k = byName.get(a); k <= byName.get(b); k++) {
        if (items[k].kind !== 'impl') moduleOf.set(k, mod);
      }
    } else if (entry.startsWith('?')) {
      // Optional: an item only some trees have (a pending upstream PR adds it); skipped when absent.
      if (byName.has(entry.slice(1))) moduleOf.set(byName.get(entry.slice(1)), mod);
    } else if (!byName.has(entry)) missing.push(entry);
    else moduleOf.set(byName.get(entry), mod);
  }
}
if (missing.length) {
  console.error('canvas-split: manifest items not found in canvas.rs: ' + missing.join(', '));
  process.exit(1);
}
// impl blocks follow their type.
items.forEach((it, idx) => {
  if (it.kind !== 'impl') return;
  const t = byName.get(it.name.slice(5));
  if (t !== undefined && moduleOf.has(t)) moduleOf.set(idx, moduleOf.get(t));
});

// --- Rewrite an item for its new module. ---
function moved(it) {
  const out = lines.slice(it.start, it.end + 1);
  const h = it.head - it.start;
  if (it.kind === 'mod') {
    // An inline module: its body, dedented.
    return lines.slice(it.head + 1, it.end).map((s) => (s.startsWith('    ') ? s.slice(4) : s));
  }
  if (it.kind !== 'impl' && !/^pub\b/.test(out[h])) out[h] = 'pub(super) ' + out[h];
  if (it.kind === 'struct' && /\{\s*$/.test(lines[it.head])) {
    for (let k = h + 1; k < out.length - 1; k++) {
      out[k] = out[k].replace(/^ {4}([a-z_][a-z0-9_]*):/, '    pub(super) $1:');
    }
  }
  if (it.kind === 'impl' && !it.forTrait) {
    for (let k = h + 1; k < out.length - 1; k++) {
      out[k] = out[k].replace(/^ {4}((?:const |unsafe |async )*fn )/, '    pub(super) $1');
    }
  }
  return out;
}

const mods = Object.keys(MANIFEST);
const keep = [];
const bodies = Object.fromEntries(mods.map((m) => [m, []]));
let lastUse = -1;
items.forEach((it, idx) => { if (it.kind === 'use') lastUse = idx; });
let cursor = 0;
items.forEach((it, idx) => {
  const mod = moduleOf.get(idx);
  if (mod === undefined) {
    keep.push(...lines.slice(cursor, it.end + 1));
  } else {
    keep.push(...lines.slice(cursor, it.start));
    if (it.kind === 'mod') {
      // The declaration stays where the inline module was, with its attributes and doc.
      keep.push(...lines.slice(it.start, it.head), lines[it.head].replace(/\s*\{\s*$/, ';'));
    }
    bodies[mod].push(moved(it));
  }
  cursor = it.end + 1;
  if (idx === lastUse) {
    keep.push('');
    for (const m of mods) {
      if (MANIFEST[m][0] === null) continue;
      keep.push(`mod ${m};`);
    }
    for (const m of mods) {
      if (MANIFEST[m][0] === null) continue;
      keep.push(`${REEXPORT.has(m) ? 'pub(crate) ' : ''}use self::${m}::*;`);
    }
  }
});
keep.push(...lines.slice(cursor));

fs.mkdirSync(dir, { recursive: true });
for (const m of mods) {
  const [doc] = MANIFEST[m];
  const parts = bodies[m].map((b) => b.join('\n'));
  const text = doc === null ? parts.join('\n\n') : `//! ${doc}\n\nuse super::*;\n\n${parts.join('\n\n')}`;
  fs.writeFileSync(path.join(dir, `${m}.rs`), text.replace(/\n*$/, '\n'));
}
fs.writeFileSync(file, keep.join('\n').replace(/\n{3,}/g, '\n\n').replace(/\n*$/, '\n'));

const left = items.filter((it, idx) => !moduleOf.has(idx) && it.kind !== 'use').map((it) => it.name);
console.log(`canvas-split: moved ${moduleOf.size} items into ${mods.length} modules; left in canvas.rs: ${left.join(', ')}`);
