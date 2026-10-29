# Tests de bout en bout d'avero-sync contre un vrai serveur IMAP/SMTP local
# (GreenMail), sans droits administrateur et sans toucher au magasin de
# certificats de Windows. Voir docs/e2e-local.md.
#
# Usage : powershell -ExecutionPolicy Bypass -File scripts\e2e-local.ps1
#
# 1. telecharge GreenMail (Maven Central, SHA-1 verifie) s'il manque ;
# 2. cree une autorite de test et un certificat pour localhost/127.0.0.1
#    s'ils manquent ;
# 3. demarre GreenMail (IMAPS 3993, SMTPS 3465) avec deux comptes dont les
#    mots de passe sont tires au hasard a chaque lancement ;
# 4. lance les tests ignores de crates/avero-sync/tests/e2e_local.rs ;
# 5. arrete GreenMail (seulement le processus java lance ici).

param(
    [string]$WorkDir = (Join-Path $env:TEMP 'avero-e2e'),
    [int]$ImapPort = 3993,
    [int]$SmtpPort = 3465,
    # GreenMail always starts its REST API too; moved off the usual 8080.
    [int]$ApiPort = 3080
)

$ErrorActionPreference = 'Stop'
$GreenMailVersion = '2.1.15'
$Repo = Split-Path -Parent $PSScriptRoot

function Find-OpenSsl {
    $cmd = Get-Command openssl -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($candidate in @(
            "$env:ProgramFiles\Git\mingw64\bin\openssl.exe",
            "$env:ProgramFiles\Git\usr\bin\openssl.exe")) {
        if (Test-Path $candidate) { return $candidate }
    }
    throw 'openssl introuvable (installez Git pour Windows ou ajoutez openssl au PATH)'
}

function Invoke-OpenSsl([string[]]$OpenSslArgs) {
    # openssl writes its progress on stderr, which Windows PowerShell would
    # turn into errors: only the exit code counts.
    $ErrorActionPreference = 'Continue'
    $output = & $script:OpenSsl @OpenSslArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        $output | ForEach-Object { Write-Host $_ }
        throw "openssl $($OpenSslArgs[0]) a echoue"
    }
}

function Test-Port([int]$Port) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $client.Connect('127.0.0.1', $Port)
        return $true
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

function New-Secret {
    $bytes = New-Object byte[] 18
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    return [Convert]::ToBase64String($bytes).Replace('+', 'A').Replace('/', 'B')
}

New-Item -ItemType Directory -Force $WorkDir | Out-Null

# 1. GreenMail
$Jar = Join-Path $WorkDir "greenmail-standalone-$GreenMailVersion.jar"
if (-not (Test-Path $Jar)) {
    $url = "https://repo1.maven.org/maven2/com/icegreen/greenmail-standalone/$GreenMailVersion/greenmail-standalone-$GreenMailVersion.jar"
    Write-Host "Telechargement de GreenMail $GreenMailVersion..."
    $partial = "$Jar.part"
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $partial
    $expected = (Invoke-WebRequest -UseBasicParsing -Uri "$url.sha1").Content
    if ($expected -is [byte[]]) { $expected = [Text.Encoding]::ASCII.GetString($expected) }
    $expected = $expected.Trim().Split(' ')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA1 $partial).Hash.ToLower()
    if ($actual -ne $expected) {
        Remove-Item $partial
        throw "SHA-1 de GreenMail inattendu ($actual au lieu de $expected)"
    }
    Move-Item $partial $Jar
}

# 2. Certificats (autorite de test + certificat serveur, valables 2 ans)
$script:OpenSsl = Find-OpenSsl
$Ca = Join-Path $WorkDir 'ca.pem'
$CaKey = Join-Path $WorkDir 'ca.key'
$ServerKey = Join-Path $WorkDir 'server.key'
$ServerCsr = Join-Path $WorkDir 'server.csr'
$ServerPem = Join-Path $WorkDir 'server.pem'
$ServerExt = Join-Path $WorkDir 'server.ext'
$Keystore = Join-Path $WorkDir 'server.p12'
# Mot de passe du magasin de cles jetable, sans valeur hors de ce dossier.
$KeystorePassword = 'avero-e2e'
if (-not (Test-Path $Keystore)) {
    Write-Host 'Creation des certificats de test...'
    Invoke-OpenSsl @('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-sha256', '-days', '730',
        '-keyout', $CaKey, '-out', $Ca, '-subj', '/CN=Avero E2E test CA',
        '-addext', 'basicConstraints=critical,CA:TRUE',
        '-addext', 'keyUsage=critical,keyCertSign,cRLSign')
    Invoke-OpenSsl @('req', '-newkey', 'rsa:2048', '-nodes', '-sha256',
        '-keyout', $ServerKey, '-out', $ServerCsr, '-subj', '/CN=localhost')
    Set-Content -Encoding ascii -Path $ServerExt -Value @(
        'basicConstraints=critical,CA:FALSE',
        'keyUsage=critical,digitalSignature,keyEncipherment',
        'extendedKeyUsage=serverAuth',
        'subjectAltName=DNS:localhost,IP:127.0.0.1')
    Invoke-OpenSsl @('x509', '-req', '-sha256', '-days', '730', '-in', $ServerCsr,
        '-CA', $Ca, '-CAkey', $CaKey, '-CAcreateserial', '-extfile', $ServerExt, '-out', $ServerPem)
    Invoke-OpenSsl @('pkcs12', '-export', '-name', 'greenmail', '-inkey', $ServerKey,
        '-in', $ServerPem, '-certfile', $Ca, '-out', $Keystore, '-passout', "pass:$KeystorePassword")
}

# 3. GreenMail
foreach ($port in @($ImapPort, $SmtpPort, $ApiPort)) {
    if (Test-Port $port) { throw "Le port $port est deja utilise : arretez ce qui l'occupe." }
}
$Domain = 'localhost.test'
$Alice = "alice@$Domain"
$Bob = "bob@$Domain"
$AlicePassword = New-Secret
$BobPassword = New-Secret
$javaArgs = @(
    # A port property alone starts that one service (no plain IMAP/SMTP).
    "-Dgreenmail.imaps.port=$ImapPort",
    "-Dgreenmail.smtps.port=$SmtpPort",
    "-Dgreenmail.api.port=$ApiPort",
    '-Dgreenmail.hostname=127.0.0.1',
    "-Dgreenmail.users=alice:$AlicePassword@$Domain,bob:$BobPassword@$Domain",
    '-Dgreenmail.users.login=email',
    "-Dgreenmail.tls.keystore.file=$Keystore",
    "-Dgreenmail.tls.keystore.password=$KeystorePassword",
    "-Dgreenmail.tls.key.password=$KeystorePassword",
    '-jar', $Jar)
$Log = Join-Path $WorkDir 'greenmail.log'
$ErrLog = Join-Path $WorkDir 'greenmail.err.log'
Write-Host "Demarrage de GreenMail (journal : $Log)..."
$java = Start-Process -FilePath 'java' -ArgumentList $javaArgs -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput $Log -RedirectStandardError $ErrLog

$exitCode = 1
try {
    $deadline = (Get-Date).AddSeconds(60)
    while (-not ((Test-Port $ImapPort) -and (Test-Port $SmtpPort))) {
        if ($java.HasExited) { throw "GreenMail s'est arrete (voir $Log et $ErrLog)" }
        if ((Get-Date) -gt $deadline) { throw "GreenMail ne repond pas (voir $Log)" }
        Start-Sleep -Milliseconds 300
    }

    # 4. Tests
    $env:AVERO_TEST_CA_PEM = $Ca
    $env:AVERO_E2E_HOST = '127.0.0.1'
    $env:AVERO_E2E_IMAP_PORT = "$ImapPort"
    $env:AVERO_E2E_SMTP_PORT = "$SmtpPort"
    $env:AVERO_E2E_ALICE = $Alice
    $env:AVERO_E2E_ALICE_PASSWORD = $AlicePassword
    $env:AVERO_E2E_BOB = $Bob
    $env:AVERO_E2E_BOB_PASSWORD = $BobPassword
    Push-Location $Repo
    try {
        # cargo writes its progress on stderr: only the exit code counts.
        $ErrorActionPreference = 'Continue'
        & cargo test -p avero-sync --features test-ca --test e2e_local -- --ignored --test-threads=1
        $exitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
} finally {
    # 5. Arret de GreenMail
    if (-not $java.HasExited) { Stop-Process -Id $java.Id -Force }
    foreach ($name in @('AVERO_E2E_ALICE_PASSWORD', 'AVERO_E2E_BOB_PASSWORD')) {
        Remove-Item "Env:$name" -ErrorAction SilentlyContinue
    }
}

if ($exitCode -eq 0) { Write-Host 'Tests de bout en bout : OK' } else { Write-Host 'Tests de bout en bout : ECHEC' }
exit $exitCode
