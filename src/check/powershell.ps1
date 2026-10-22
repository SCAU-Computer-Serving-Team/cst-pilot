$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$version = [Version]'1.24.0'
if ($env:CST_PSSCRIPTANALYZER_MODULE) {
    $manifest = Test-ModuleManifest -Path $env:CST_PSSCRIPTANALYZER_MODULE
    if ($manifest.Version -ne $version) { throw "PSScriptAnalyzer must be $version" }
    Import-Module -Name $manifest.Path -Force
} else {
    Import-Module PSScriptAnalyzer -RequiredVersion $version -ErrorAction Stop
}
Push-Location $root
try {
    $files = @(& git ls-files --cached --others --exclude-standard -- '*.ps1' '*.psm1' '*.psd1' | Sort-Object -Unique)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot enumerate PowerShell sources' }
    $failures = [Collections.Generic.List[object]]::new()
    foreach ($file in $files) {
        $tokens = $null
        $parseErrors = $null
        $null = [Management.Automation.Language.Parser]::ParseFile((Join-Path $root $file), [ref]$tokens, [ref]$parseErrors)
        foreach ($errorRecord in $parseErrors) {
            $failures.Add("${file}:$($errorRecord.Extent.StartLineNumber): $($errorRecord.Message)")
        }
        foreach ($diagnostic in @(Invoke-ScriptAnalyzer -Path (Join-Path $root $file) -Severity Error,Warning)) {
            $failures.Add("${file}:$($diagnostic.Line): $($diagnostic.RuleName): $($diagnostic.Message)")
        }
    }
    if ($failures.Count) { throw ($failures -join [Environment]::NewLine) }
    Write-Output "PowerShell: $($files.Count) files passed parser and PSScriptAnalyzer $version"
} finally { Pop-Location }
