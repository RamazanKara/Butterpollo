"""One-time PIN pairing and the serverinfo extensions Artemis reads.

Starts an isolated host on port 48723 (no streaming, no display or audio
changes), then checks:
- unpaired serverinfo: no MAC, Permission 0, virtual display and frame limiter
  extensions, the PC name as host name;
- POST /api/otp and pairing with otpauth = SHA-256(PIN + salt + passphrase),
  without entering a PIN in the web interface, under the OTP device name;
- a wrong otpauth still answers getservercert but cannot finish pairing;
- paired serverinfo: Permission and the configured ServerCommand names.

Usage: otp_pairing.py <host exe> <work directory>
"""
import hashlib, json, os, pathlib, secrets, socket, subprocess, sys, time
import xml.etree.ElementTree as ET
from datetime import datetime, timedelta, timezone
import requests, urllib3
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa, padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.x509.oid import NameOID

urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
host_exe, work = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
profile = work / 'config'
(profile / 'logs').mkdir(parents=True, exist_ok=True)
port = 48723
web, http, https = f'https://127.0.0.1:{port+1}', f'http://127.0.0.1:{port}', f'https://127.0.0.1:{port-5}'
failures = []

def check(name, ok, detail=''):
    print(f"{'PASS' if ok else 'FAIL'} {name} {detail}", flush=True)
    if not ok:
        failures.append(name)

(profile / 'sunshine.conf').write_text(f'''port = {port}
system_tray = false
update_check_interval = 0
upnp = false
enable_discovery = false
stream_audio = false
virtual_display_mode = per_client
server_cmd = [{{"name":"Lock PC","cmd":"rundll32 user32.dll,LockWorkStation"}},{{"name":"Sleep","cmd":"echo sleep"}}]
''')
salt = secrets.token_hex(8)
(profile / 'sunshine_state.json').write_text(json.dumps({'username': 'test', 'salt': salt, 'password': hashlib.sha256(('rust-smoke-only' + salt).encode()).digest()[::-1].hex().upper()}))
(profile / 'apps.json').write_text(json.dumps({'apps': [{'name': 'Desktop', 'cmd': ''}]}))
env = dict(os.environ, RUST_LOG='info', NO_PROXY='127.0.0.1,localhost')
log = (work / 'host.log').open('w')
host = subprocess.Popen([str(host_exe), '--config-dir', str(profile), '--bind', '127.0.0.1', '--no-tray'], stdout=log, stderr=subprocess.STDOUT, env=env)
try:
    for _ in range(100):
        try:
            requests.get(http + '/serverinfo', timeout=2)
            break
        except requests.RequestException:
            time.sleep(0.2)
    info = ET.fromstring(requests.get(http + '/serverinfo', timeout=30).text)
    field = info.findtext
    check('unpaired_hides_mac', field('mac') == '00:00:00:00:00:00', field('mac'))
    check('unpaired_permission_zero', field('Permission') == '0', field('Permission'))
    check('virtual_display_extensions', field('VirtualDisplayCapable') == 'true' and field('VirtualDisplayHDRCapable') == 'true' and field('VirtualDisplayDriverReady') in ('true', 'false'),
          f"ready={field('VirtualDisplayDriverReady')}")
    check('frame_limiter_extensions', field('FrameLimiterSupported') == '1' and field('VirtualDisplayFrameLimiterEnabled') == '1' and field('FrameLimiterFpsLimitMilliHz') == '0',
          f"enabled={field('FrameLimiterEnabled')}")
    check('host_name_is_pc_name', field('hostname').lower() == socket.gethostname().lower(), field('hostname'))
    check('current_game_uuid_present', info.find('currentgameuuid') is not None)
    check('unpaired_lists_no_commands', not info.findall('ServerCommand'))

    session = requests.Session()
    session.verify = False
    login = session.post(web + '/api/auth/login', json={'username': 'test', 'password': 'rust-smoke-only'}, timeout=10)
    login.raise_for_status()
    session.headers['X-CSRF-Token'] = login.json()['csrf_token']
    check('short_passphrase_refused', session.post(web + '/api/otp', json={'passphrase': 'abc'}, timeout=10).status_code >= 400)

    def pair_client(uid, auth_for):
        """Pair with otpauth. auth_for(pin_salt_hex) returns the otpauth value; the
        PIN used for the AES key is the one the client believes in."""
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, uid)])
        cert = x509.CertificateBuilder().subject_name(subject).issuer_name(subject).public_key(key.public_key()).serial_number(x509.random_serial_number()).not_valid_before(datetime.now(timezone.utc) - timedelta(days=1)).not_valid_after(datetime.now(timezone.utc) + timedelta(days=30)).sign(key, hashes.SHA256())
        cert_pem = cert.public_bytes(serialization.Encoding.PEM)
        key_pem = key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption())
        pair_salt = os.urandom(16)
        otpauth, pin = auth_for(pair_salt.hex())
        aes = hashlib.sha256(pair_salt + pin.encode()).digest()[:16]
        def ecb(data, encrypt=True):
            c = Cipher(algorithms.AES(aes), modes.ECB())
            op = c.encryptor() if encrypt else c.decryptor()
            return op.update(data) + op.finalize()
        def pair(args):
            r = requests.get(http + '/pair', params={'uniqueid': uid, **args}, timeout=20)
            root = ET.fromstring(r.text)
            if root.attrib.get('status_code') != '200' or root.findtext('paired') != '1':
                raise RuntimeError(r.text)
            return root
        root = pair({'phrase': 'getservercert', 'salt': pair_salt.hex(), 'clientcert': cert_pem.hex(), 'devicename': uid, 'otpauth': otpauth})
        server = x509.load_pem_x509_certificate(bytes.fromhex(root.findtext('plaincert')))
        challenge = os.urandom(16)
        root = pair({'clientchallenge': ecb(challenge).hex()})
        response = ecb(bytes.fromhex(root.findtext('challengeresponse')), False)
        secret = os.urandom(16)
        proof = hashlib.sha256(response[32:] + cert.signature + secret).digest()
        root = pair({'serverchallengeresp': ecb(proof).hex()})
        server_proof = bytes.fromhex(root.findtext('pairingsecret'))
        if response[:32] != hashlib.sha256(challenge + server.signature + server_proof[:16]).digest():
            raise RuntimeError('server proof mismatch (wrong PIN)')
        pair({'clientpairingsecret': (secret + key.sign(secret, padding.PKCS1v15(), hashes.SHA256())).hex()})
        return cert_pem, key_pem

    otp = session.post(web + '/api/otp', json={'passphrase': 'living-room', 'deviceName': 'OTP phone'}, timeout=10).json()
    check('otp_created', otp.get('status') is True and len(otp.get('otp', '')) == 4 and otp['otp'].isdigit(), json.dumps(otp))
    pin = otp['otp']
    try:
        pair_client('otp-wrong-' + secrets.token_hex(3), lambda s: (hashlib.sha256((pin + s + 'wrong').encode()).hexdigest().upper(), pin))
        check('wrong_passphrase_cannot_pair', False, 'pairing completed')
    except RuntimeError as error:
        check('wrong_passphrase_cannot_pair', True, str(error)[:80])
    uid = 'otp-' + secrets.token_hex(3)
    cert_pem, key_pem = pair_client(uid, lambda s: (hashlib.sha256((pin + s + 'living-room').encode()).hexdigest().upper(), pin))
    clients = session.get(web + '/api/clients/list', timeout=10).json()['clients']
    check('otp_paired_with_device_name', any(c['name'] == 'OTP phone' for c in clients), json.dumps([c['name'] for c in clients]))
    try:
        pair_client('otp-reuse-' + secrets.token_hex(3), lambda s: (hashlib.sha256((pin + s + 'living-room').encode()).hexdigest().upper(), pin))
        check('otp_is_single_use', False, 'second pairing completed')
    except RuntimeError as error:
        check('otp_is_single_use', True, str(error)[:80])

    (work / 'client.pem').write_bytes(cert_pem)
    (work / 'client-key.pem').write_bytes(key_pem)
    client = requests.Session()
    client.verify = False
    client.cert = (str(work / 'client.pem'), str(work / 'client-key.pem'))
    paired = next(c for c in clients if c['name'] == 'OTP phone')
    info = ET.fromstring(client.get(https + '/serverinfo', timeout=30).text)
    check('paired_permission', info.findtext('Permission') == str(paired['perm']), f"{info.findtext('Permission')} vs {paired['perm']}")
    commands = [c.text for c in info.findall('ServerCommand')]
    expect_commands = bool(paired['perm'] & 0x00100000)
    check('server_commands_follow_permission', commands == (['Lock PC', 'Sleep'] if expect_commands else []), json.dumps(commands))
    check('paired_status', info.findtext('PairStatus') == '1')
finally:
    host.terminate()
    host.wait(timeout=20)
print('OTP', 'PASS' if not failures else 'FAIL ' + ','.join(failures), flush=True)
sys.exit(1 if failures else 0)
