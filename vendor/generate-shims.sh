#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
R="/Users/philippremy/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f"
Z="$(pwd)/vendor/zed"
S="$(pwd)/vendor/gpui-pre-shims"
mkdir -p "$S"

MAP=(
"gpui-pre:gpui:crates/gpui"
"gpui-pre-apple:gpui_apple:crates/gpui_apple"
"gpui-pre-collections:collections:crates/collections"
"gpui-pre-derive-refineable:derive_refineable:crates/refineable/derive_refineable"
"gpui-pre-http-client:http_client:crates/http_client"
"gpui-pre-linux:gpui_linux:crates/gpui_linux"
"gpui-pre-macos:gpui_macos:crates/gpui_macos"
"gpui-pre-macros:gpui_macros:crates/gpui_macros"
"gpui-pre-perf:perf:tooling/perf"
"gpui-pre-platform:gpui_platform:crates/gpui_platform"
"gpui-pre-refineable:refineable:crates/refineable"
"gpui-pre-scheduler:scheduler:crates/scheduler"
"gpui-pre-shared-string:gpui_shared_string:crates/gpui_shared_string"
"gpui-pre-sum-tree:sum_tree:crates/sum_tree"
"gpui-pre-util:gpui_util:crates/gpui_util"
"gpui-pre-util-macros:util_macros:crates/util_macros"
"gpui-pre-web:gpui_web:crates/gpui_web"
"gpui-pre-wgpu:gpui_wgpu:crates/gpui_wgpu"
"gpui-pre-windows:gpui_windows:crates/gpui_windows"
"gpui-pre-zlog:zlog:crates/zlog"
"gpui-pre-ztracing:ztracing:crates/ztracing"
"gpui-pre-ztracing-macro:ztracing_macro:crates/ztracing_macro"
)

for entry in "${MAP[@]}"; do
  pkg="${entry%%:*}"
  rest="${entry#*:}"
  dirname="${rest%%:*}"
  realrel="${rest#*:}"
  wrapdir="$S/$dirname"
  mkdir -p "$wrapdir"
  realpath_abs="$Z/$realrel"
  if [ ! -d "$realpath_abs" ]; then
    echo "MISSING: $realpath_abs"
    continue
  fi
  shopt -s dotglob nullglob
  for item in "$realpath_abs"/*; do
    base="$(basename "$item")"
    if [ "$base" = "Cargo.toml" ]; then continue; fi
    # gpui-kit's own packaging script (script/bump-gpui.ts) injects a
    # facade-path-rewriting module into gpui_macros specifically, wrapping
    # every proc-macro entry point in gpui_macros.rs so bare `gpui::`/
    # `gpui_platform::` paths in derive-macro OUTPUT get rewritten to
    # `::gpui_kit::`/`crate::` at macro-expansion time (via
    # proc_macro_crate::crate_name("gpui-kit")). This is NOT part of Zed's
    # real crates/gpui_macros/ — without it, every derive consumed through
    # the gpui-kit facade (which is all of dtb-ke-ui) fails with "cannot
    # find crate `gpui`" because the bare path never gets rewritten. Use
    # the crates.io-published (facade-patched) copy of this one file
    # instead of symlinking Zed's real one; everything else in this crate
    # is confirmed byte-identical and stays symlinked.
    if [ "$pkg" = "gpui-pre-macros" ] && [ "$base" = "src" ]; then
      mkdir -p "$wrapdir/src"
      for f in "$item"/*; do
        fbase="$(basename "$f")"
        if [ "$fbase" = "gpui_macros.rs" ]; then continue; fi
        rel="$(python3 -c "import os,sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))" "$f" "$wrapdir/src")"
        ln -sfn "$rel" "$wrapdir/src/$fbase"
      done
      cp "$R/$pkg-0.3.6/src/gpui_macros.rs" "$wrapdir/src/gpui_macros.rs"
      cp "$R/$pkg-0.3.6/src/gpui_pre_facade_paths.rs" "$wrapdir/src/gpui_pre_facade_paths.rs"
      continue
    fi
    rel="$(python3 -c "import os,sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))" "$item" "$wrapdir")"
    ln -sfn "$rel" "$wrapdir/$base"
  done
  shopt -u dotglob nullglob
  cp "$R/$pkg-0.3.6/Cargo.toml" "$wrapdir/Cargo.toml"
  # Vendored upstream code: silence its warnings (cargo only hides them for registry/git deps).
  printf '\n# dtb-ke: vendored upstream code; its warnings are not ours to fix.\n[lints.rust.warnings]\nlevel = "allow"\npriority = -1\n' >> "$wrapdir/Cargo.toml"
  echo "OK: $pkg -> $dirname (from $realrel)"
done
