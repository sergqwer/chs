# chs installer for Windows (PowerShell 5.1 or 7):
#
#   irm https://raw.githubusercontent.com/sergqwer/chs/main/install.ps1 | iex
#
# Installs the latest release into %LOCALAPPDATA%\Programs\chs for the current user -
# no admin rights - and runs it once, which adds that folder to the user PATH, starts
# the tray and registers it to start with Windows. Run it again to update.
# $env:CHS_VERSION = 'v0.2.1' pins a release, $env:CHS_INSTALL_DIR changes the folder.

& {
    $ErrorActionPreference = 'Stop'
    # The progress bar makes Invoke-WebRequest many times slower in Windows PowerShell.
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'sergqwer/chs'
    $asset = 'chs-windows-x86_64.zip'
    $release = if ($env:CHS_VERSION) { "download/$env:CHS_VERSION" } else { 'latest/download' }
    $url = "https://github.com/$repo/releases/$release/$asset"
    $dir = if ($env:CHS_INSTALL_DIR) { $env:CHS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\chs' }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('chs-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Host "Downloading $url"
        Invoke-WebRequest -Uri $url -OutFile "$tmp\chs.zip" -UseBasicParsing
        Expand-Archive -Path "$tmp\chs.zip" -DestinationPath "$tmp\x" -Force

        # A running tray holds its exe open. Only this session's: never another user's.
        $session = (Get-Process -Id $PID).SessionId
        $trays = @(Get-Process chs-tray -ErrorAction SilentlyContinue | Where-Object SessionId -eq $session)
        if ($trays) {
            $trays | Stop-Process -Force
            $trays | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
        }

        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        Copy-Item -Path "$tmp\x\*" -Destination $dir -Recurse -Force
        Get-ChildItem $dir -Filter *.exe | Unblock-File
    }
    finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    # The first run does the integration: user PATH, tray, autostart, default config.
    # Its notes go to stderr, which Windows PowerShell would turn into errors.
    $ErrorActionPreference = 'Continue'
    & "$dir\chs.exe" --list -n 0 2>&1 | ForEach-Object { Write-Host "$_" }
    $ErrorActionPreference = 'Stop'

    if (-not (($env:Path -split ';') -contains $dir)) { $env:Path = "$env:Path;$dir" }
    Write-Host "Installed $(& "$dir\chs.exe" --version) to $dir"
}
