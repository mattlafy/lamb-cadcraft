# Golden fixtures — LAMB's core behaviour as language-neutral JSON

Every call the tests of `geometry-primitives`, `document-model` and `derivation-engine` make into
those packages, recorded as JSON input → JSON output. Two jobs:

1. **A spec for a port.** A reimplementation in another language (the Rust `lamb-core` in
   [docs/07-architecture/cadcraft-integration.md](../../../docs/07-architecture/cadcraft-integration.md))
   is correct when it replays every case here. The TypeScript tests stay the human-readable intent;
   these files are what a port can actually run.
2. **A regression net.** `tools/golden/replay.test.ts` replays them against the TypeScript in the
   normal suite, so a behaviour change the packages' own assertions don't pin down still shows up.

## Regenerating

```bash
npx vitest run --config tools/golden/vitest.capture.config.ts
```

This reruns the three packages' tests with every exported function wrapped
(`tools/golden/capture.setup.ts`), rewriting these directories from scratch. Capture is
byte-stable, so the git diff after a recapture is exactly the behaviour that changed. **Recapture
after an intended behaviour change, review the diff, and commit it with the change.** A replay
failure without an intended change is a regression.

Only calls made by the tests themselves are recorded, not calls one package function makes to
another. So coverage is the set of exports the tests call directly: about 95 of the ~180 exports
in the three packages, in about 900 cases. A function with no case here is still tested through its
callers, but a port has no direct fixture for it. To cover one, write an ordinary test for it in its
package and recapture.

## Layout

`<package>/<test file>.json`. For example, `derivation-engine/resolve.json` comes from
`packages/derivation-engine/src/resolve.test.ts`.

```jsonc
{
  "format": "lamb-golden/1",
  "source": "packages/derivation-engine/src/resolve.test.ts",
  "cases": [
    {
      "test": "describe name > it name",            // where the call came from
      "module": "derivation-engine/resolve",          // <package>/<source file without .ts>
      "export": "resolveCutSegment",                 // the exported function called
      "args": [ … ],                                  // positional arguments, encoded (below)
      "result": …,                                    // the return value, encoded and canonical
      "throws": "message",                            // instead of result, if the call threw
      "argsAfter": [ … ]                              // only if the call mutated its arguments
    }
  ]
}
```

A case appears once per distinct `(module, export, args)` in each file. Files are written one case
per line (compact JSON), so a recapture diff lists exactly the cases that changed.

## Value encoding

Plain JSON, with object keys sorted. JSON can't express a few things, so they become tagged
objects:

| Value | Encoded as |
|---|---|
| `undefined` (an argument or array element) | `{"$undefined": true}` |
| `undefined` (an object property) | key omitted — an absent key and an undefined one are the same |
| `NaN`, `Infinity`, `-Infinity` | `{"$number": "NaN"}` etc. |
| `Map` | `{"$map": [[key, value], …]}` |
| `Set` | `{"$set": [value, …]}` |

Calls involving functions, class instances or cyclic values aren't recorded (only two at present).

## Replaying a case

1. Decode `args` and call `export` from `module` with them.
2. If `throws` is present, the call must throw, with that message. A port may map the message to its
   own error values, but it must fail on the same inputs.
3. Otherwise, encode the return value and the arguments afterwards, **canonicalize** both together
   (below), and compare against `result` and `argsAfter`. If `argsAfter` is absent, the arguments
   must be unchanged.

### Canonical form

- **Minted ids.** A string the call created with `generateId` (shape
  `<prefix>-<base36 ≥8 chars>-<base36>`) that **appears nowhere in `args`** is replaced by `$id:0`,
  `$id:1`, … in order of first appearance. Walk `result` first, then `argsAfter`, with object keys in
  sorted order. A port mints ids however it likes. Only which values are the same id has to match.
- **Time.** The clock is frozen at `2026-01-01T00:00:00.000Z` during capture. A port must treat that
  as "now" when replaying, so a stamped `createdAt` matches literally.

### Comparison

- Numbers are equal when `|a − b| ≤ 1e-9 · max(1, |a|, |b|)`. That's loose enough for a different
  language's `atan2` or `sin`, and tight enough that a 1 ppm change fails.
- A missing key equals `null`.
- Everything else is exact. Array order matters.

The reference implementation of all of the above is `tools/golden/codec.ts`.
