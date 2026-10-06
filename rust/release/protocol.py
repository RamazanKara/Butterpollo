"""Protocol checks against a packaged host, isolated profile on port 48523.

1. A pairing abandoned after serverchallengeresp (what Moonlight does after a
   wrong PIN) does not block the next pairing from the same unique ID.
2. Pairing the same certificate again keeps one device entry and renames it.
3. An odd launch size (1279x719) is accepted.
4. /launch on the Resume tile answers <gamesession>, not <resume>.

usage: protocol.py --package DIR --work DIR
Writes WORK/protocol/result.json.
"""
import argparse, concurrent.futures, hashlib, json, os, pathlib, shutil, subprocess, sys, time
import xml.etree.ElementTree as ET
from datetime import datetime, timedelta, timezone
import requests, urllib3
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.x509.oid import NameOID
urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

parser = argparse.ArgumentParser()
parser.add_argument('--package', required=True, type=pathlib.Path)
parser.add_argument('--work', required=True, type=pathlib.Path)
args = parser.parse_args()
build = args.package

plain = requests.Session(); plain.trust_env = False
info = ET.fromstring(plain.get('http://127.0.0.1:47989/serverinfo', timeout=3).text)
assert info.findtext('state') == 'SUNSHINE_SERVER_FREE', 'the installed host is streaming'

case = args.work / 'protocol'
if case.exists():
    shutil.rmtree(case)
profile = case / 'config'; (profile / 'logs').mkdir(parents=True)
env = os.environ.copy(); env.update(PATH=str(build) + ';' + env['PATH'], RUST_LOG='info', NO_PROXY='*')
diag = json.loads(subprocess.check_output([str(build / 'butterpollo.exe'), '--diagnostics'], env=env, text=True))
monitor = next(m for m in diag['monitors'] if m['primary'])
(profile / 'sunshine.conf').write_text('\n'.join([
    'port = 48523', 'encoder = amf', 'capture = wgc', f"output_name = {monitor['device_id']}",
    'virtual_display_mode = disabled', 'dd_configuration_option = disabled', 'dd_resolution_option = disabled',
    'dd_refresh_rate_option = disabled', 'dd_hdr_option = disabled', 'dd_always_restore_from_golden = false',
    'frame_limiter_enable = false', 'stream_audio = false', 'upnp = false', 'enable_discovery = false',
    'vulkan_hdr_layer = false', 'system_tray = false', 'update_check_interval = 0', 'pyrowave = false', '']))
(profile / 'apps.json').write_text(json.dumps({'apps': [{'name': 'Desktop', 'cmd': '', 'virtual-display': False}]}))
# The test fixture's account (rust/tests/interop.py), in the previous host's format.
salt = os.urandom(8).hex()
(profile / 'sunshine_state.json').write_text(json.dumps({'username': 'test', 'salt': salt, 'password': hashlib.sha256(('rust-smoke-only' + salt).encode()).digest()[::-1].hex().upper()}))

log_file = (case / 'host.stdout.log').open('w')
host = subprocess.Popen([str(build / 'butterpollo.exe'), '--config-dir', str(profile), '--assets', str(build / 'assets' / 'web'), '--bind', '127.0.0.1', '--no-tray'],
                        stdout=log_file, stderr=subprocess.STDOUT, env=env, cwd=case, creationflags=subprocess.CREATE_NO_WINDOW)
http, https, web = 'http://127.0.0.1:48523', 'https://127.0.0.1:48518', 'https://127.0.0.1:48524'
checks = {}
try:
    log = profile / 'logs' / 'butterpollo.log'
    deadline = time.monotonic() + 60
    while not (log.exists() and 'encoder capability probe completed' in log.read_text(errors='replace')):
        assert host.poll() is None and time.monotonic() < deadline, 'host not ready'
        time.sleep(.2)
    session = requests.Session(); session.verify = False; session.trust_env = False
    login = session.post(web + '/api/auth/login', json={'username': 'test', 'password': 'rust-smoke-only'}, timeout=10)
    login.raise_for_status(); session.headers['X-CSRF-Token'] = login.json()['csrf_token']

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'Protocol probe')])
    now = datetime.now(timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key())
            .serial_number(x509.random_serial_number()).not_valid_before(now - timedelta(days=1))
            .not_valid_after(now + timedelta(days=30)).sign(key, hashes.SHA256()))
    cert_pem = cert.public_bytes(serialization.Encoding.PEM)
    (case / 'client.pem').write_bytes(cert_pem)
    (case / 'client-key.pem').write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    uid = '0123456789ABCDEF'  # Moonlight's fixed unique ID

    def pair(device, abandon=False):
        salt = os.urandom(16); pin = '%04d' % (int.from_bytes(os.urandom(2), 'big') % 10000)
        aes = hashlib.sha256(salt + pin.encode()).digest()[:16]
        def ecb(data, encrypt=True):
            c = Cipher(algorithms.AES(aes), modes.ECB()); op = c.encryptor() if encrypt else c.decryptor(); return op.update(data) + op.finalize()
        def step(params):
            r = plain.get(http + '/pair', params={'uniqueid': uid, **params}, timeout=20)
            root = ET.fromstring(r.text)
            assert root.attrib.get('status_code') == '200' and root.findtext('paired') == '1', r.text
            return root
        with concurrent.futures.ThreadPoolExecutor() as ex:
            future = ex.submit(step, {'phrase': 'getservercert', 'salt': salt.hex(), 'clientcert': cert_pem.hex(), 'devicename': device})
            time.sleep(.4)
            session.post(web + '/api/pin', json={'pin': pin, 'name': device, 'uniqueid': uid}, timeout=10).raise_for_status()
            root = future.result(timeout=20)
        challenge = os.urandom(16)
        root = step({'clientchallenge': ecb(challenge).hex()}); response = ecb(bytes.fromhex(root.findtext('challengeresponse')), False)
        secret = os.urandom(16); proof = hashlib.sha256(response[32:] + cert.signature + secret).digest()
        step({'serverchallengeresp': ecb(proof).hex()})
        if not abandon:
            step({'clientpairingsecret': (secret + key.sign(secret, padding.PKCS1v15(), hashes.SHA256())).hex()})

    pair('Abandoned', abandon=True)
    try:
        pair('Phone')
        checks['pairing after an abandoned attempt'] = True
    except AssertionError as error:
        checks['pairing after an abandoned attempt'] = False
        print('pairing refused:', str(error)[:200])
        raise
    pair('Phone again')
    devices = [c for c in session.get(web + '/api/clients/list', timeout=10).json()['clients'] if c['name'] in ('Phone', 'Phone again', 'Abandoned')]
    checks['re-pairing keeps one renamed entry'] = [c['name'] for c in devices] == ['Phone again']
    paired = devices[0]
    session.post(web + '/api/clients/update', json={'uuid': paired['uuid'], 'perm': 0x071f1f00}, timeout=10).raise_for_status()

    client = requests.Session(); client.verify = False; client.trust_env = False
    client.cert = (str(case / 'client.pem'), str(case / 'client-key.pem'))
    apps = ET.fromstring(client.get(https + '/applist', timeout=10).text)
    desktop = next(a.findtext('ID') for a in apps.findall('App') if a.findtext('AppTitle').endswith('Desktop'))
    common = {'rikey': bytes(range(16)).hex(), 'rikeyid': '123', 'corever': '1'}
    first = ET.fromstring(client.get(https + '/launch', params={'appid': desktop, 'mode': '1279x719x60', **common}, timeout=20).text)
    checks['odd size launch accepted'] = first.attrib.get('status_code') == '200' and first.findtext('gamesession') == '1'
    resumed = ET.fromstring(client.get(https + '/launch', params={'appid': '2147483501', **common}, timeout=20).text)
    checks['/launch on the Resume tile replies gamesession'] = resumed.find('gamesession') is not None and resumed.find('resume') is None
    client.get(https + '/cancel', timeout=10)
    session.post(web + '/api/clients/unpair', json={'uuid': paired['uuid']}, timeout=10)
finally:
    host.terminate()
    try: host.wait(timeout=10)
    except subprocess.TimeoutExpired: host.kill(); host.wait()
    log_file.close()
    (case / 'result.json').write_text(json.dumps(dict(passed=bool(checks) and all(checks.values()), checks=checks), indent=2))
for k, v in checks.items():
    print(('PASS ' if v else 'FAIL ') + k)
sys.exit(0 if checks and all(checks.values()) else 1)
