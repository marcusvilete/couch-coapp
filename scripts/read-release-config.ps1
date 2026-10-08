# Reads the private release configuration (M7): relay_url, relay_token, invite_page, test.
# Dot-sourced by build-installer.ps1 and relay.ps1; not run on its own. The file lives outside
# the repository (`cargo xtask release-config` creates it); COUCH_RELEASE_CONFIG points at another one.

function Get-ReleaseConfigPath {
    if ($env:COUCH_RELEASE_CONFIG) { return $env:COUCH_RELEASE_CONFIG }
    return Join-Path $env:USERPROFILE '.couch-coapp\release.toml'
}

# A flat TOML file of `key = "text"` and `key = true|false` lines; anything else is refused.
function Read-ReleaseConfig([string] $Path) {
    $config = @{ relay_url = ''; relay_token = ''; invite_page = ''; test = $false }
    foreach ($line in Get-Content -LiteralPath $Path -Encoding UTF8) {
        $line = $line.Trim()
        if ($line -eq '' -or $line.StartsWith('#')) { continue }
        if ($line -match '^(\w+)\s*=\s*"([^"]*)"$') {
            $key, $value = $Matches[1], $Matches[2]
        } elseif ($line -match '^(\w+)\s*=\s*(true|false)$') {
            $key, $value = $Matches[1], ($Matches[2] -eq 'true')
        } else {
            throw "${Path}: can't read the line '$line'"
        }
        if (-not $config.ContainsKey($key)) { throw "${Path}: unknown setting '$key'" }
        # `test = "false"` (quoted) would read as true: each key takes only its own type.
        if ($config[$key].GetType() -ne $value.GetType()) {
            $wanted = if ($config[$key] -is [bool]) { 'true or false, without quotes' } else { 'text in quotes' }
            throw "${Path}: '$key' takes $wanted"
        }
        $config[$key] = $value
    }
    return $config
}

# The configuration must never be committed: refuse one inside the repository.
function Assert-OutsideRepo([string] $Path, [string] $Repo) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetFullPath($Repo).TrimEnd('\') + '\'
    if ($full.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "$full is inside the repository. Keep release.toml outside it (see deploy\release.example.toml)."
    }
}
