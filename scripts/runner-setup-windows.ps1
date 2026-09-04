# Sets up the Windows Forgejo Actions runner — run *inside* the Windows VM
# scripts/provision-windows-vm.sh created (a real Windows environment is
# required here, not a container: gpui's Windows backend shells out to
# fxc.exe, the classic Direct3D HLSL shader compiler (Shader Model 5.x/DXBC,
# what Direct3D 11 uses — not dxc.exe, the newer DXIL/SM6+ compiler, which
# has a cross-platform build and isn't what gpui wants here), at build time
# — see RUNNERS.md).
#
# Handles: Rust + the *-pc-windows-gnullvm targets, llvm-mingw (the gnullvm
# cross toolchain — x86_64 native, aarch64 cross-linked from this same x86_64
# VM), the Windows SDK (for fxc.exe), WiX Toolset v4 (.msi), Go (to build
# forgejo-runner from source — no Windows binary is published), zipsign, and
# the runner service itself.
#
# Idempotent — checks before installing, safe to re-run.
#
#   powershell -ExecutionPolicy Bypass -File runner-setup-windows.ps1
#
# IMPORTANT — the eventual runner service almost certainly does NOT run as
# the interactive user this script is run as: NSSM (or any Windows service)
# defaults to running as LocalSystem, whose %USERPROFILE% is a completely
# different, unrelated profile. rustup's default ~/.cargo/~/.rustup and
# `dotnet tool install --global`'s ~/.dotnet/tools are both per-user
# locations that a LocalSystem-run service would never see — the exact same
# class of bug as the Linux/macOS scripts had (a per-user install invisible
# to the identity that actually runs the daemon), just with a different
# mechanism. Fix here: install everything machine-wide, under this same
# $ToolsDir, and persist both PATH *and* CARGO_HOME/RUSTUP_HOME at Machine
# (not just session/User) scope — a fresh service process rebuilds its
# environment from the registry's System variables, so that's what has to
# carry these, not whatever the current PowerShell session happens to have.

$ErrorActionPreference = "Stop"
$Have = { param($cmd) [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }
function Set-MachinePath($prependDir) {
    $current = [Environment]::GetEnvironmentVariable("Path", "Machine")
    if ($current -notlike "*$prependDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$prependDir;$current", "Machine")
    }
    $env:Path = "$prependDir;$env:Path"
}

function Step($msg) { Write-Host "`n-- $msg --" -ForegroundColor Cyan }
function Confirm($prompt) {
    $r = Read-Host "$prompt [y/N]"
    return $r -match '^[Yy]'
}

$ToolsDir = "C:\ci-tools"
New-Item -ItemType Directory -Force -Path $ToolsDir | Out-Null

# -- 1. Rust -----------------------------------------------------------------
# CARGO_HOME/RUSTUP_HOME redirected to $ToolsDir (machine-wide) instead of
# rustup's default %USERPROFILE%\.cargo -- see the IMPORTANT note above.
# rustup's proxies (cargo.exe, rustc.exe, ...) re-read these env vars on
# every invocation, so persisting them at Machine scope is what actually
# makes the installed toolchain visible to a LocalSystem-run service later,
# not just to this session.
Step "Rust toolchain"
$CargoHome = "$ToolsDir\cargo"
$RustupHome = "$ToolsDir\rustup"
[Environment]::SetEnvironmentVariable("CARGO_HOME", $CargoHome, "Machine")
[Environment]::SetEnvironmentVariable("RUSTUP_HOME", $RustupHome, "Machine")
$env:CARGO_HOME = $CargoHome
$env:RUSTUP_HOME = $RustupHome
Set-MachinePath "$CargoHome\bin"

if (-not (Test-Path "$CargoHome\bin\rustc.exe")) {
    if (Confirm "rustup not found -- download + run rustup-init.exe?") {
        $rustupExe = "$env:TEMP\rustup-init.exe"
        Invoke-WebRequest "https://win.rustup.rs/x86_64" -OutFile $rustupExe
        & $rustupExe -y --default-toolchain nightly
    }
} else {
    Write-Host "rustc already installed: $(& "$CargoHome\bin\rustc.exe" --version)"
}
rustup toolchain install nightly | Out-Null
rustup target add x86_64-pc-windows-gnullvm aarch64-pc-windows-gnullvm --toolchain nightly

# -- 2. llvm-mingw (the gnullvm cross toolchain) ---------------------------
Step "llvm-mingw"
$LlvmMingwDir = "$ToolsDir\llvm-mingw"
if (Test-Path "$LlvmMingwDir\bin\clang.exe") {
    Write-Host "llvm-mingw already present at $LlvmMingwDir"
} elseif (Confirm "Download + extract llvm-mingw (ucrt runtime, x86_64 host)?") {
    $release = (Invoke-RestMethod "https://api.github.com/repos/mstorsjo/llvm-mingw/releases/latest")
    $asset = $release.assets | Where-Object { $_.name -match "x86_64\.zip$" -and $_.name -match "ucrt" } | Select-Object -First 1
    $zip = "$env:TEMP\llvm-mingw.zip"
    Invoke-WebRequest $asset.browser_download_url -OutFile $zip
    Expand-Archive -Path $zip -DestinationPath $ToolsDir -Force
    Get-ChildItem $ToolsDir -Directory -Filter "llvm-mingw-*" | Select-Object -First 1 | ForEach-Object {
        Rename-Item $_.FullName $LlvmMingwDir
    }
    Remove-Item $zip
}
$env:Path = "$LlvmMingwDir\bin;$env:Path"
[Environment]::SetEnvironmentVariable("Path", "$LlvmMingwDir\bin;" + [Environment]::GetEnvironmentVariable("Path", "Machine"), "Machine")

# -- 3. Windows SDK (fxc.exe) -----------------------------------------------
# fxc.exe (the classic Direct3D HLSL compiler, Shader Model 5.x/DXBC), not
# dxc.exe -- confirmed against a real build (see RUNNERS.md).
Step "Windows SDK (Direct3D HLSL shader compiler)"
# The SDK ships fxc.exe under separate x86/x64/arm64 subdirectories -- it
# runs as a *host* tool inside the build script regardless of which Rust
# target triple is being built (this VM is x86_64, whether building
# x86_64-pc-windows-gnullvm natively or cross-building aarch64), so it must
# be the x64 copy specifically.
$allFxc = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter "fxc.exe" -ErrorAction SilentlyContinue
$fxcCandidates = $allFxc | Where-Object { $_.FullName -match '\\x64\\' }
if (-not $fxcCandidates) {
    # No x64 copy found at all -- fall back to whatever exists rather than
    # treat this as "not installed", but this is a red flag worth seeing.
    $fxcCandidates = $allFxc
    if ($fxcCandidates) {
        Write-Warning "No x64 fxc.exe found under the Windows SDK -- falling back to $($fxcCandidates[0].FullName), which may be the wrong architecture."
    }
}
if ($fxcCandidates) {
    $fxcPath = $fxcCandidates[0].FullName
    Write-Host "fxc.exe found: $fxcPath"
    Set-MachinePath $fxcCandidates[0].DirectoryName
    # PATH alone isn't reliable here -- read gpui_windows's actual build.rs
    # (crates/gpui_windows/build.rs in the zed-industries/zed checkout) to
    # confirm this rather than guess further: its find_fxc_compiler() falls
    # back to `where.exe fxc.exe` and does `.trim()` on the *entire* output
    # as if it's always a single path -- but where.exe prints one match per
    # PATH directory containing fxc.exe, one per line, and Set-MachinePath
    # only ever prepends (an earlier, unfiltered run of this script already
    # left a wrong-architecture fxc.exe directory on PATH, so there are two
    # matches now). The multi-line blob that produces isn't a valid path at
    # all, which is why the failures were a raw CreateProcess-level error
    # (Command::output()'s Err(e) branch -- gpui's build script never got as
    # far as fxc.exe actually running) rather than anything from fxc.exe
    # itself. GPUI_FXC_PATH is checked first, before that where.exe call, so
    # setting it here to the exact resolved path sidesteps the bug
    # entirely regardless of how many fxc.exe copies end up on PATH.
    [Environment]::SetEnvironmentVariable("GPUI_FXC_PATH", $fxcPath, "Machine")
    $env:GPUI_FXC_PATH = $fxcPath
} elseif (Confirm "fxc.exe not found -- download + run the Windows SDK installer (needs a few GB, several minutes)?") {
    $sdkExe = "$env:TEMP\winsdksetup.exe"
    Invoke-WebRequest "https://go.microsoft.com/fwlink/?linkid=2237912" -OutFile $sdkExe
    Start-Process -FilePath $sdkExe -ArgumentList "/features + /q" -Wait
    Write-Host "SDK installed -- re-run this script to pick up fxc.exe and set GPUI_FXC_PATH."
} else {
    Write-Warning "Without the SDK, gpui's Windows build (which shells out to fxc.exe for HLSL) will fail."
}

# -- 4. .NET SDK + WiX Toolset v4 (.msi) -----------------------------------
Step ".NET SDK + WiX Toolset v4"
if (-not (& $Have dotnet)) {
    if (Confirm "dotnet SDK not found -- download + run the installer?") {
        $dotnetExe = "$env:TEMP\dotnet-install.ps1"
        Invoke-WebRequest "https://dot.net/v1/dotnet-install.ps1" -OutFile $dotnetExe
        & $dotnetExe -Channel LTS -InstallDir "$ToolsDir\dotnet"
        $env:Path = "$ToolsDir\dotnet;$env:Path"
        [Environment]::SetEnvironmentVariable("Path", "$ToolsDir\dotnet;" + [Environment]::GetEnvironmentVariable("Path", "Machine"), "Machine")
    }
} else {
    Write-Host "dotnet already installed: $(dotnet --version)"
}
# --tool-path (a machine-wide shared directory), not --global (which installs
# under %USERPROFILE%\.dotnet\tools -- invisible to a LocalSystem-run
# service; see the IMPORTANT note at the top of this file). The `wix
# extension add -g` call below is believed (not verified against a real
# LocalSystem service run) to store the extension scoped to *this specific*
# wix.exe -- i.e. still machine-wide, since this wix.exe itself now lives
# under $ToolsDir rather than a per-user location -- but if `wix build` ever
# fails to find WixToolset.UI.wixext specifically when run as a service
# despite `wix --version` working fine, this is the first thing to check.
$WixToolPath = "$ToolsDir\dotnet-tools"
Set-MachinePath $WixToolPath
if (-not (Test-Path "$WixToolPath\wix.exe")) {
    if (Confirm "Install the WiX Toolset v4 CLI (dotnet tool, machine-wide)?") {
        dotnet tool install --tool-path $WixToolPath wix
        & "$WixToolPath\wix.exe" extension add -g WixToolset.UI.wixext
    }
} else {
    Write-Host "wix already installed: $(& "$WixToolPath\wix.exe" --version)"
}

# -- 4b. CET workaround for wix.exe ------------------------------------------
# A KVM guest's virtualization of CET's actual runtime machinery (the
# shadow-stack MSRs, not just the CPUID bit) is much less mature than plain
# CPUID passthrough -- wix.exe's .NET-built, CET-shadow-stack-marked apphost
# fails outright with "Your System does not fully support CET" as a result,
# even though the physical host CPU (Tiger Lake) genuinely supports CET. A
# hypervisor-side fix (stripping the CET feature bits from the passed-through
# CPU model) was tried first and turned out to be unnecessary -- this
# process-mitigation override is what actually fixed it (see RUNNERS.md).
# It's a persistent, machine-wide registry policy (Image File Execution
# Options) that Windows' loader consults for any launch of wix.exe regardless
# of what starts it, so this only needs setting once per machine -- but
# Set-ProcessMitigation is itself idempotent (safe to re-run), so just always
# re-assert it here rather than detecting the current state first.
Step "CET workaround for wix.exe"
Set-ProcessMitigation -Name wix.exe -Disable UserShadowStack
Write-Host "UserShadowStack disabled for wix.exe"

# -- 5. Go (to build forgejo-runner from source -- no Windows binary ships) -
Step "Go (for building forgejo-runner)"
if (-not (& $Have go)) {
    if (Confirm "Download + install Go?") {
        $goMsi = "$env:TEMP\go.msi"
        $goVer = (Invoke-RestMethod "https://go.dev/VERSION?m=text").Split("`n")[0]
        Invoke-WebRequest "https://go.dev/dl/$goVer.windows-amd64.msi" -OutFile $goMsi
        Start-Process msiexec.exe -ArgumentList "/i `"$goMsi`" /quiet" -Wait
        # The MSI already installs machine-wide (Program Files) -- just
        # needed on PATH at Machine scope, same reasoning as everything else
        # in this file.
        Set-MachinePath "C:\Program Files\Go\bin"
    }
} else {
    Write-Host "go already installed: $(go version)"
}

# -- 6. zipsign -------------------------------------------------------------
Step "zipsign"
if (-not (& $Have zipsign)) {
    if (Confirm "cargo install zipsign?") { cargo install zipsign }
} else {
    Write-Host "zipsign already installed"
}

# -- 6b. CI cache directory (fixed target-dir for Tip's incremental builds) -
# Forgejo checks each job out into a fresh, random per-job directory -- see
# RUNNERS.md § target-dir caching. tip.yml points CARGO_TARGET_DIR at this
# fixed path so cargo's build output survives between runs instead of being
# reset by every job's fresh checkout.
Step "CI cache directory"
$CiCacheRoot = "C:\Codeberg\DTB-Kampfrichtereinsatzplaene"
if (Test-Path $CiCacheRoot) {
    Write-Host "already present: $CiCacheRoot"
} elseif (Confirm "Create $CiCacheRoot for tip.yml's persistent build cache?") {
    New-Item -ItemType Directory -Force -Path $CiCacheRoot | Out-Null
}

# -- 7. forgejo-runner (built from source) ----------------------------------
Step "forgejo-runner (source build)"
$RunnerExe = "$ToolsDir\forgejo-runner.exe"
if (Test-Path $RunnerExe) {
    Write-Host "forgejo-runner already built at $RunnerExe"
} elseif (Confirm "Clone + build forgejo-runner from source (needs Go)?") {
    $src = "$ToolsDir\forgejo-runner-src"
    if (-not (Test-Path $src)) {
        git clone --depth 1 https://code.forgejo.org/forgejo/runner.git $src
    }
    Push-Location $src
    go build -o $RunnerExe .
    Pop-Location
}

# -- 8. registration + service ----------------------------------------------
# Codeberg's "Create new Runner" page (repo -> Settings -> Actions -> Runners)
# generates a UUID + token pair and shows the exact command to run -- there is
# no separate `register` step any more: `forgejo-runner daemon` takes
# --url/--uuid/--token-url directly. The token goes in a file (not inline on
# the command line) -- Codeberg's own example uses a file:// URL for exactly
# that reason. See RUNNERS.md § Registering a runner.
#
# NOTE: registration tokens are single-use and short-lived -- if a daemon
# attempt with one fails for any reason, get a fresh UUID+token pair from
# Codeberg before trying again; retrying the same one fails with
# "runner registration token not found".
Step "Registration"
$RunnerDir = "$ToolsDir\runner-data"
New-Item -ItemType Directory -Force -Path $RunnerDir | Out-Null
$TokenFile = "$RunnerDir\runner-token"
$ConfigFile = "$RunnerDir\runner-config.json"

if ((Test-Path $TokenFile) -and (Test-Path $ConfigFile)) {
    Write-Host "already configured ($TokenFile + $ConfigFile exist) -- delete both to reconfigure"
} elseif (Confirm "Configure this runner now? (you'll need the UUID + token Codeberg shows you when you create a new runner: repo -> Settings -> Actions -> Runners -> Create new Runner)") {
    $instance = Read-Host "Instance URL [https://codeberg.org/]"
    if ([string]::IsNullOrWhiteSpace($instance)) { $instance = "https://codeberg.org/" }
    $uuid = Read-Host "Runner UUID"
    $token = Read-Host "Runner token"

    # No BOM, no trailing newline -- WriteAllText matches `echo -n` on the
    # other two platforms' scripts.
    [System.IO.File]::WriteAllText($TokenFile, $token)
    @{ url = $instance; uuid = $uuid; label = "windows-host:host" } | ConvertTo-Json | Set-Content -Path $ConfigFile -NoNewline
}

$cfg = if (Test-Path $ConfigFile) { Get-Content $ConfigFile -Raw | ConvertFrom-Json } else { $null }
if ($cfg) {
    # file:// needs forward slashes even on Windows.
    $tokenUri = "file:///" + ($TokenFile -replace '\\', '/')
    $daemonArgs = "daemon --url $($cfg.url) --uuid $($cfg.uuid) --token-url $tokenUri --label $($cfg.label)"

    Write-Host "`nTo run the daemon now (foreground, for testing):"
    Write-Host "  cd $RunnerDir; & `"$RunnerExe`" $daemonArgs"
    Write-Host "`nFor a real always-on service, register it as a Windows service, e.g. with NSSM"
    Write-Host "(the default LocalSystem account is fine -- everything above was installed"
    Write-Host "machine-wide specifically so that works; see the IMPORTANT note at the top"
    Write-Host "of this file):"
    Write-Host "  nssm install forgejo-runner `"$RunnerExe`" $daemonArgs"
    Write-Host "  nssm set forgejo-runner AppDirectory `"$RunnerDir`""
    Write-Host "  nssm start forgejo-runner"
} else {
    Write-Host "`nNot configured yet -- re-run this script and choose to configure the runner."
}
