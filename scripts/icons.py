#!/usr/bin/env python3
"""Icon pipeline: an Xcode 26 Icon Composer master -> every shipped platform icon.

Ports `crates/dtb-ke-bundle/src/icon.rs`. macOS-only, deliberately -- Icon Composer's Liquid Glass
rendering has no portable equivalent, only Apple's own `actool`/`iconutil`/`ictool` (Xcode 26+) can
compile it, so this needs no cross-platform story the way the other ported scripts do. Requires
Pillow (`pip install Pillow`) for the parts that *are* portable image work (resizing, ICO/PNG
encoding) -- everything Liquid-Glass-specific still shells out to Apple's own tools, matching the
original Rust exactly.

The master is `assets/icons/{AppIcon,DebuggerIcon}.icon`, an Icon Composer package (`icon.json` +
source images). Compiled into:
  - `AppIcon.icns`   -- a flattened, multi-size standalone icon (the pre-Tahoe `CFBundleIconFile`
                        fallback).
  - `Assets.car`     -- the compiled asset catalog carrying the actual Liquid Glass icon, looked up
                        via `CFBundleIconName` (must equal APP_ICON_NAME below).
  - `AppIcon.png`     -- a flat 1024x1024 raster extracted from the .icns's largest rendition -- the
                        one bridge back to plain pixels every other derivation (.ico, Linux hicolor,
                        the app's own About window) works from.
  - `AppIcon.ico`, `hicolor/<size>/apps/<rdns-id>.png`, `icon-512.png` -- pure Pillow from that PNG.
  - `ios/` (app product only) -- the iOS variant of the same Assets.car/icns/PNG story, plus the
    About window's flattened iOS renditions (via Icon Composer's own `ictool`).

A separate flat master (`assets/icons/{FileIcon,CrashDumpIcon}.png`) drives the `.dtbke`/`.dtbkedmp`
document-type icon the same way, minus the Liquid Glass/Assets.car step (a document icon needs no
material layering) -- only its `.icns` step needs `iconutil`; the rest is pure Pillow.

Outputs are committed to `assets/icons/**/generated/` as regular git-tracked files, not a
`target/`-cached build artifact -- Icon Composer needs Xcode 26+, so CI hosts without it just consume
what's already checked in. Run this and commit the result whenever a `.icon` master or a flat
document-icon master changes.

    icons.py [--product app|debugger]
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from PIL import Image

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent
APP_ICON_NAME = "AppIcon"
MACOS_MIN_VERSION = "11.0"
IOS_MIN_VERSION = "15.0"

HICOLOR_SIZES = [16, 22, 24, 32, 48, 64, 96, 128, 192, 256, 512]
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
IOS_ABOUT_ICON_PX = 320


@dataclass
class Product:
    icon_master: str  # workspace-relative .icon package
    icon_dir: str  # workspace-relative source dir (its own generated/ lives below it)
    doc_icon_master: str  # workspace-relative flat PNG master for the document-type icon
    rdns_id: str  # ASCII reverse-DNS id -- the hicolor apps/ icon name
    doc_mime_type: str  # -> doc_mime_icon_name() (freedesktop mimetypes/ icon name)
    ios: bool


PRODUCTS = {
    "app": Product(
        icon_master="assets/icons/AppIcon.icon",
        icon_dir="assets/icons",
        doc_icon_master="assets/icons/FileIcon.png",
        rdns_id="de.philippremy.DTB-Kampfrichtereinsatzplaene",
        doc_mime_type="application/x-dtbke",
        ios=True,
    ),
    "debugger": Product(
        icon_master="assets/icons/DebuggerIcon.icon",
        icon_dir="assets/icons/debugger",
        doc_icon_master="assets/icons/CrashDumpIcon.png",
        rdns_id="de.philippremy.DTB-Kampfrichtereinsatzplaene-Debugger",
        doc_mime_type="application/x-dtbkedmp",
        ios=False,
    ),
}


# --- paths -----------------------------------------------------------------------------------


def master_dir(p: Product) -> Path:
    return WORKSPACE_ROOT / p.icon_master


def source_dir(p: Product) -> Path:
    return WORKSPACE_ROOT / p.icon_dir


def generated_dir(p: Product) -> Path:
    return source_dir(p) / "generated"


def scratch_dir() -> Path:
    """Deliberately outside `generated_dir()` so a crash mid-generation can never leave stray files
    for `git add` to pick up."""
    return WORKSPACE_ROOT / "target/bundle/icon-scratch"


def icns_path(p: Product) -> Path:
    return generated_dir(p) / "AppIcon.icns"


def assets_car_path(p: Product) -> Path:
    return generated_dir(p) / "Assets.car"


def flat_png_path(p: Product) -> Path:
    return generated_dir(p) / "AppIcon.png"


def ico_path(p: Product) -> Path:
    return generated_dir(p) / "AppIcon.ico"


def hicolor_dir(p: Product) -> Path:
    return generated_dir(p) / "hicolor"


def png_512_path(p: Product) -> Path:
    return generated_dir(p) / "icon-512.png"


def doc_master_path(p: Product) -> Path:
    return WORKSPACE_ROOT / p.doc_icon_master


def doc_icns_path(p: Product) -> Path:
    return generated_dir(p) / "DocumentIcon.icns"


def doc_ico_path(p: Product) -> Path:
    return generated_dir(p) / "DocumentIcon.ico"


def doc_mime_icon_name(p: Product) -> str:
    return p.doc_mime_type.replace("/", "-")


def ios_dir(p: Product) -> Path:
    return generated_dir(p) / "ios"


def ios_icon_plist(p: Product) -> Path:
    return ios_dir(p) / "icon-info.plist"


def ios_about_icon_path(p: Product, dark: bool) -> Path:
    return generated_dir(p) / ("AppIcon-ios-dark.png" if dark else "AppIcon-ios.png")


def available(p: Product) -> bool:
    """Whether the committed generated icons are present -- checked by the bundler on every host;
    Windows/Linux never regenerate, they only consume what this script (macOS-only) already wrote
    and a human committed."""
    return flat_png_path(p).exists()


def doc_available(p: Product) -> bool:
    return doc_ico_path(p).exists()


# --- fs helpers --------------------------------------------------------------------------------


def fresh_dir(path: Path) -> None:
    shutil.rmtree(path, ignore_errors=True)
    path.mkdir(parents=True, exist_ok=True)


def copy_tree(src: Path, dst: Path) -> None:
    shutil.copytree(src, dst)


# --- rasterisation (Pillow) --------------------------------------------------------------------


def rasterise(master: Image.Image, size: int) -> Image.Image:
    """`master` scaled to fit within `size`x`size` (aspect preserved) and centred on a transparent
    canvas. For a square master (the flattened Icon Composer output) the fit is exact, so this is
    just a plain resize -- compositing unconditionally, even when nothing needs letterboxing, would
    alpha-blend every partially-transparent edge pixel onto the canvas and premultiply its RGB, a
    real (if subtle) corruption of the app icon's anti-aliased edges. For a non-square one (the
    document icon's page silhouette) it letterboxes instead of stretching."""
    w, h = master.size
    scale = min(size / w, size / h)
    new_w = max(1, min(size, round(w * scale)))
    new_h = max(1, min(size, round(h * scale)))
    resized = master.resize((new_w, new_h), Image.LANCZOS)
    if new_w == size and new_h == size:
        return resized
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.paste(resized, ((size - new_w) // 2, (size - new_h) // 2), resized)
    return canvas


def write_png(img: Image.Image, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path, format="PNG")


# --- Windows .ico ------------------------------------------------------------------------------


def build_ico(master: Image.Image, out: Path) -> None:
    """Pillow writes a real multi-resolution ICO natively (PNG-compressed frames, which the ICO
    format has allowed since Windows Vista and WiX/modern Windows read fine) -- no hand-rolled
    ICONDIR writer needed here, unlike the original Rust (no equivalent library there).

    Pillow's ICO writer (`IcoImagePlugin._save`) silently drops every requested size *larger* than
    the base image it's called on (`width, height = im.size`; any `size` bigger than that is
    `continue`d past) -- it does not treat `sizes`/`append_images` as "here are the pre-made frames,
    just write them". So the base image must be the *largest* frame, appended frames the rest, in
    descending order; each still gets matched to its own pre-rasterised frame by exact `.size`
    rather than Pillow re-thumbnailing it from the base."""
    out.parent.mkdir(parents=True, exist_ok=True)
    sizes_desc = sorted(ICO_SIZES, reverse=True)
    frames = [rasterise(master, s) for s in sizes_desc]
    frames[0].save(out, format="ICO", sizes=[(s, s) for s in sizes_desc], append_images=frames[1:])


# --- Linux hicolor -------------------------------------------------------------------------------


def build_hicolor_named(master: Image.Image, root: Path, category: str, name: str) -> None:
    """Writes into `root/<size>x<size>/<category>/<name>.png` for every HICOLOR_SIZES rendition.
    Doesn't touch any other category already on disk -- callers that want a clean rebuild wipe the
    generated dir themselves first (`generate_app_icon` does, freshening the whole tree)."""
    for size in HICOLOR_SIZES:
        d = root / f"{size}x{size}" / category
        d.mkdir(parents=True, exist_ok=True)
        write_png(rasterise(master, size), d / f"{name}.png")


# --- macOS tool wrappers -------------------------------------------------------------------------


def run_actool(master: Path, scratch: Path) -> None:
    """Compile the `.icon` master into `scratch/{AppIcon.icns,Assets.car}`.

    `actool` can report success (exit 0) while writing nothing at all -- e.g. omitting
    `--output-partial-info-plist` degrades app-icon compilation to a silent no-op "notice" rather
    than an error -- so the real check is that the expected output file landed, not just the exit
    status. `--standalone-icon-behavior all` is required for a *complete* fallback `.icns`: the
    default only emits a couple of representative sizes (16/128), not something fit to ship as the
    pre-Tahoe icon."""
    partial_plist = scratch / "partial.plist"
    result = subprocess.run(
        [
            "actool",
            "--compile", str(scratch),
            "--platform", "macosx",
            "--minimum-deployment-target", MACOS_MIN_VERSION,
            "--app-icon", APP_ICON_NAME,
            "--standalone-icon-behavior", "all",
            "--errors", "--warnings", "--notices",
            "--output-partial-info-plist", str(partial_plist),
            str(master),
        ],
        cwd=WORKSPACE_ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0 or not (scratch / "AppIcon.icns").exists():
        raise RuntimeError(
            f"actool did not produce AppIcon.icns/Assets.car (exit {result.returncode}):\n{result.stdout}"
        )


def extract_flat_png(p: Product, scratch: Path) -> Image.Image:
    """Convert the fallback `.icns` to an `.iconset` and pull out its largest (1024x1024,
    `icon_512x512@2x.png`) rendition as the flat master everything else derives from."""
    iconset = scratch / "AppIcon.iconset"
    result = subprocess.run(
        ["iconutil", "--convert", "iconset", "--output", str(iconset), str(icns_path(p))],
        cwd=WORKSPACE_ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"iconutil: {result.stderr}")
    largest = iconset / "icon_512x512@2x.png"
    shutil.copy2(largest, flat_png_path(p))
    return Image.open(largest).convert("RGBA")


def build_icns_from_flat(master: Image.Image, out: Path) -> None:
    """Build an `.icns` from a flat master image via a hand-built `.iconset` -- the reverse of
    `extract_flat_png`. Only needs `iconutil` (ships with the Xcode Command Line Tools), unlike the
    app icon's `actool`/Icon Composer pipeline."""
    scratch = scratch_dir() / "doc-icon-src"
    fresh_dir(scratch)
    iconset = scratch / "DocumentIcon.iconset"
    iconset.mkdir(parents=True, exist_ok=True)

    # Apple's `.iconset` naming convention: (pixel size, file name).
    icns_sizes = [
        (16, "icon_16x16.png"), (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"), (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png"),
    ]
    for size, name in icns_sizes:
        write_png(rasterise(master, size), iconset / name)

    result = subprocess.run(
        ["iconutil", "--convert", "icns", "--output", str(out), str(iconset)],
        cwd=WORKSPACE_ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"iconutil: {result.stderr}")
    shutil.rmtree(scratch, ignore_errors=True)


def generate_ios_app_icon(p: Product, master: Path, scratch: Path) -> None:
    """Compile the iOS icon into `ios_dir()`. Built for iphoneos; the simulator loads the same
    catalog."""
    work = scratch / "ios-actool"
    fresh_dir(work)
    partial = work / "partial.plist"
    result = subprocess.run(
        [
            "xcrun", "actool", "--compile", str(work),
            "--platform", "iphoneos",
            "--minimum-deployment-target", IOS_MIN_VERSION,
            "--target-device", "ipad", "--app-icon", APP_ICON_NAME,
            "--include-all-app-icons", "--errors", "--warnings", "--notices",
            "--output-partial-info-plist", str(partial),
            str(master),
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0 or not (work / "Assets.car").exists() or not partial.exists():
        raise RuntimeError(f"actool did not compile the iOS icon (exit {result.returncode}): {result.stdout}")

    dest = ios_dir(p)
    fresh_dir(dest)
    for entry in work.iterdir():
        if entry.name == "partial.plist":
            shutil.copy2(entry, ios_icon_plist(p))
        elif entry.is_file():
            shutil.copy2(entry, dest / entry.name)


def ictool_path() -> Path | None:
    """Icon Composer's command-line renderer, shipped inside Xcode (`Icon Composer.app` in the
    developer directory's parent `Applications/`)."""
    result = subprocess.run(["xcode-select", "-p"], capture_output=True, text=True)
    if result.returncode != 0:
        return None
    developer = Path(result.stdout.strip())
    for candidate in [
        developer / "../Applications/Icon Composer.app/Contents/Executables/ictool",
        Path("/Applications/Icon Composer.app/Contents/Executables/ictool"),
    ]:
        if candidate.exists():
            return candidate
    return None


def generate_ios_about_icons(p: Product, master: Path, scratch: Path) -> None:
    """The About window's iOS icons: the Default and Dark renditions of the Icon Composer master,
    rendered by Icon Composer's own `ictool` edge to edge with only the squircle corners
    transparent (unlike AppIcon.png, which is the macOS icon with its margin and shadow)."""
    ictool = ictool_path()
    if ictool is None:
        raise RuntimeError("Icon Composer's `ictool` was not found (needs Xcode 26+)")
    fresh_dir(scratch)
    for rendition, dark in [("Default", False), ("Dark", True)]:
        full = scratch / f"ios-{rendition}.png"
        result = subprocess.run(
            [
                str(ictool), str(master),
                "--export-image", "--output-file", str(full),
                "--platform", "iOS", "--rendition", rendition,
                "--width", "1024", "--height", "1024", "--scale", "1",
            ],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0 or not full.exists():
            raise RuntimeError(f"ictool did not render the {rendition} iOS icon:\n{result.stderr}")
        image = Image.open(full).convert("RGBA")
        small = image.resize((IOS_ABOUT_ICON_PX, IOS_ABOUT_ICON_PX), Image.LANCZOS)
        write_png(small, ios_about_icon_path(p, dark))
    shutil.rmtree(scratch, ignore_errors=True)


# --- top-level orchestration --------------------------------------------------------------------


def generate_app_icon(p: Product) -> bool:
    """The app-icon half of `generate()` -- the Icon Composer master -> .icns/Assets.car/.ico/
    hicolor apps/ PNGs. `False` (no error) when there's no master to build from yet."""
    master = master_dir(p)
    if not master.exists():
        print(f"icons.py: no icon master at {master} — bundling without an icon", file=sys.stderr)
        return False

    scratch = scratch_dir()
    fresh_dir(scratch)

    # actool names the compiled icon after the `.icon` package, and the bundle expects `AppIcon`
    # (APP_ICON_NAME) -- stage a copy under that name so a differently named master
    # (DebuggerIcon.icon) works too.
    staged = scratch / "in" / "AppIcon.icon"
    copy_tree(master, staged)
    run_actool(staged, scratch)

    out_dir = generated_dir(p)
    fresh_dir(out_dir)
    shutil.move(str(scratch / "AppIcon.icns"), icns_path(p))
    shutil.move(str(scratch / "Assets.car"), assets_car_path(p))

    flat = extract_flat_png(p, scratch)
    shutil.rmtree(scratch, ignore_errors=True)

    write_png(flat.resize((512, 512), Image.LANCZOS), png_512_path(p))
    build_ico(flat, ico_path(p))
    build_hicolor_named(flat, hicolor_dir(p), "apps", p.rdns_id)
    if p.ios:
        generate_ios_about_icons(p, master, scratch)
        generate_ios_app_icon(p, master, scratch)
    return True


def generate_doc_icon(p: Product) -> bool:
    """The document-icon half of `generate()` -- doc_master_path() -> .icns/.ico/hicolor
    mimetypes/ PNGs. `False` (no error) when there's no master to build from yet -- bundling
    without one just means the OS shows a generic file icon for the document type."""
    master_path = doc_master_path(p)
    if not master_path.exists():
        print(
            f"icons.py: no document-type icon master at {master_path} — bundling without a file-type icon",
            file=sys.stderr,
        )
        return False
    master = Image.open(master_path).convert("RGBA")

    generated_dir(p).mkdir(parents=True, exist_ok=True)
    build_icns_from_flat(master, doc_icns_path(p))
    build_ico(master, doc_ico_path(p))
    build_hicolor_named(master, hicolor_dir(p), "mimetypes", doc_mime_icon_name(p))
    return True


def generate(p: Product) -> bool:
    if sys.platform != "darwin":
        raise RuntimeError(
            "icon generation needs Xcode 26's actool/iconutil (Icon Composer support) — macOS "
            "only. Run icons.py on a Mac with Xcode 26+ and commit assets/icons/**/generated/."
        )
    app_icon = generate_app_icon(p)
    doc_icon = generate_doc_icon(p)
    return app_icon or doc_icon


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--product", choices=["app", "debugger"], default="app")
    args = parser.parse_args(argv[1:])

    product = PRODUCTS[args.product]
    try:
        wrote = generate(product)
    except RuntimeError as e:
        print(f"icons.py: {e}", file=sys.stderr)
        return 1
    if not wrote:
        return 1
    print(f"icons.py: icons written to {generated_dir(product)} — commit the result", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
