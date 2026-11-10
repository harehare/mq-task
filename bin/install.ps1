# mq-task installer for Windows (PowerShell 5.1+ and PowerShell 7+)
#
#   irm https://task.mqlang.org/install.ps1 | iex
#
# Settings, as environment variables (a piped script cannot take parameters):
#   MQ_TASK_VERSION         release tag to install, e.g. v0.2.4 (default: latest)
#   MQ_TASK_INSTALL_DIR     where to put mq-task.exe
#                           (default: %LOCALAPPDATA%\mq-task\bin)
#   MQ_TASK_NO_MODIFY_PATH  set to 1 to leave the user PATH alone

& {
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version 3.0
    # Windows PowerShell 5.1 still defaults to old TLS versions, and its
    # progress bar makes downloads many times slower.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $ProgressPreference = 'SilentlyContinue'

    $repo = 'harehare/mq-task'

    function Write-Step([string]$Message) { Write-Host "  $Message" -ForegroundColor Green }
    function Write-Warn([string]$Message) { Write-Host "  ! $Message" -ForegroundColor Yellow }

    if ($PSVersionTable.PSVersion.Major -ge 6 -and -not $IsWindows) {
        throw 'This installer is for Windows. On Linux and macOS use: curl -sSL https://task.mqlang.org/install.sh | bash'
    }

    # Only an x86_64 build is published for Windows; Arm64 Windows runs it
    # through its built-in emulation.
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    switch ($arch) {
        'X64'   { $target = 'x86_64-pc-windows-msvc' }
        'Arm64' { $target = 'x86_64-pc-windows-msvc'; Write-Warn 'Installing the x86_64 build (runs emulated on Arm64).' }
        default { throw "Unsupported architecture: $arch" }
    }

    $version = $env:MQ_TASK_VERSION
    if (-not $version) {
        $release = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$repo/releases/latest"
        $version = $release.tag_name
    }
    if (-not $version) { throw 'Failed to determine the latest version.' }
    if ($version -notmatch '^v?[0-9A-Za-z._-]+$') { throw "Invalid version: $version" }
    if (-not $version.StartsWith('v')) { $version = "v$version" }

    $installDir = $env:MQ_TASK_INSTALL_DIR
    if (-not $installDir) { $installDir = Join-Path $env:LOCALAPPDATA 'mq-task\bin' }

    $assetName = "mq-task-$target.exe"
    $baseUrl = "https://github.com/$repo/releases/download/$version"

    Write-Host ''
    Write-Host "Installing mq-task $version ($target)" -ForegroundColor Cyan

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("mq-task-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $exe = Join-Path $tmp $assetName
        $sums = Join-Path $tmp 'checksums.txt'

        Write-Step "Downloading $assetName"
        Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$assetName" -OutFile $exe
        Write-Step 'Downloading checksums.txt'
        Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/checksums.txt" -OutFile $sums

        # Lines look like: <sha256>  mq-task-<target>.exe/mq-task-<target>.exe
        $expected = $null
        foreach ($line in Get-Content -LiteralPath $sums) {
            $parts = $line -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].Trim().EndsWith($assetName)) {
                $expected = $parts[0].Trim()
                break
            }
        }
        if (-not $expected) { throw "No checksum found for $assetName; aborting." }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash
        # -ne is case-insensitive in PowerShell; Get-FileHash prints upper case.
        if ($actual -ne $expected) {
            throw "Checksum verification failed.`n  expected: $expected`n  got:      $actual"
        }
        Write-Step 'Checksum verified'

        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $dest = Join-Path $installDir 'mq-task.exe'
        try {
            Move-Item -Force -LiteralPath $exe -Destination $dest
        } catch {
            throw "Could not write $dest. Is mq-task running? Close it and try again. ($($_.Exception.Message))"
        }
        Write-Step "Installed to $dest"
    } finally {
        Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue
    }

    if ($env:MQ_TASK_NO_MODIFY_PATH -eq '1') {
        Write-Warn "Leaving PATH alone. Add $installDir to it yourself."
    } else {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @()
        if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ } }
        $onPath = $entries | Where-Object { $_.TrimEnd('\') -ieq $installDir.TrimEnd('\') }
        if (-not $onPath) {
            $newPath = (@($entries) + $installDir) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            Write-Step "Added $installDir to your user PATH"
        }
        # Make it usable in this session too.
        if (($env:Path -split ';') -notcontains $installDir) { $env:Path = "$env:Path;$installDir" }
    }

    Write-Host ''
    Write-Host 'mq-task is installed. Try:' -ForegroundColor Cyan
    Write-Host '  mq-task --version'
    Write-Host '  mq-task init'
    Write-Host ''
    Write-Host 'Open a new terminal if the command is not found.'
}
