"""Persistent auth and previous-session migration against a test-owned host.

No service is installed, no display mode is changed, and only this script's
subprocess is stopped. Binary must be from the packaged runtime directory.
"""
import argparse, hashlib, json, pathlib, subprocess, time, uuid
import requests, urllib3
urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
p = argparse.ArgumentParser()
p.add_argument('--binary', type=pathlib.Path, required=True)
p.add_argument('--artifacts', type=pathlib.Path, required=True)
p.add_argument('--port', type=int, default=48323)
args = p.parse_args()
binary = args.binary.resolve(strict=True)
root = args.artifacts.resolve(strict=True)
directory = root / ('auth-session-' + str(uuid.uuid4()))
directory.mkdir()
assert directory.resolve().parent == root
(directory / 'sunshine.conf').write_text('port=%d\nencoder=amf\ncapture=wgc\n'
    'virtual_display_mode=disabled\ndd_configuration_option=disabled\n'
    'dd_resolution_option=disabled\ndd_refresh_rate_option=disabled\ndd_hdr_option=disabled\n'
    'stream_audio=false\ninstall_steam_audio_drivers=false\nupnp=false\n'
    'enable_discovery=false\nvulkan_hdr_layer=false\nupdate_check_interval=0\n' % args.port)
base = 'https://127.0.0.1:%d' % (args.port + 1)
process = None
outputs = []
generation = 0
password = 'auth-fixture-only'
browser = requests.Session(); browser.verify = False
admin = requests.Session(); admin.verify = False; admin.auth = ('test', password)

def start():
    global process, generation
    generation += 1
    out = (directory / ('host-%d.stdout.log' % generation)).open('w')
    err = (directory / ('host-%d.stderr.log' % generation)).open('w')
    outputs.extend([out, err])
    process = subprocess.Popen([str(binary), '--config-dir', str(directory), '--assets',
        str(binary.parent / 'assets/web'), '--port', str(args.port), '--bind', '127.0.0.1', '--no-tray'],
        cwd=binary.parent, stdout=out, stderr=err, creationflags=subprocess.CREATE_NO_WINDOW)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AssertionError('Test-owned host exited; inspect fixture logs')
        try:
            requests.get(base + '/api/auth/status', verify=False, timeout=1).raise_for_status()
            return
        except requests.RequestException:
            time.sleep(.1)
    raise AssertionError('Test-owned host did not become ready')

def stop():
    global process
    if process is not None:
        process.terminate(); process.wait(timeout=15); process = None

def call(client, method, path, code=200, **kwargs):
    r = client.request(method, base + path, timeout=10, **kwargs)
    assert r.status_code == code, (path, r.status_code)
    return r

def document():
    return json.loads((directory / 'vibeshine_state.json').read_text())

def bearer(secret):
    client = requests.Session(); client.verify = False
    client.headers['Authorization'] = 'Bearer ' + secret
    return client

def digest(secret): return hashlib.sha256(secret.encode()).hexdigest()
def previous_digest(secret): return hashlib.sha256(secret.encode()).digest()[::-1].hex().upper()

try:
    start()
    anon = call(browser, 'GET', '/api/csrf-token').json()['csrf_token']
    call(browser, 'POST', '/api/password', headers={'X-CSRF-Token':anon}, json={
        'newUsername':'test', 'newPassword':password, 'confirmNewPassword':password})
    credentials=json.loads((directory/'sunshine_state.json').read_text())
    assert credentials['password']==previous_digest(password+credentials['salt'])
    response = call(browser, 'POST', '/api/auth/login', headers={'X-CSRF-Token':anon},
        json={'username':'TEST', 'password':password, 'remember_me':True})
    login = response.json()
    assert 'Max-Age=' in response.headers['Set-Cookie']
    row = next(s for s in document()['root']['session_tokens'] if s['hash'] == digest(login['access_token']))
    assert row['remember_me'] and row['username'] == 'test'
    deadline = row['refresh_expires_at']
    saved = json.dumps(document())
    assert login['access_token'] not in saved and login['refresh_token'] not in saved
    stop(); start()
    assert call(browser, 'GET', '/api/auth/status').json()['authenticated']
    browser.headers['X-CSRF-Token'] = call(browser, 'GET', '/api/csrf-token').json()['csrf_token']
    rotated = call(browser, 'POST', '/api/auth/refresh').json()
    row = next(s for s in document()['root']['session_tokens'] if s['hash'] == digest(rotated['access_token']))
    assert row['refresh_expires_at'] == deadline
    assert not call(bearer(login['access_token']), 'GET', '/api/auth/status').json()['authenticated']
    replay = requests.Session(); replay.verify = False
    call(replay, 'POST', '/api/auth/refresh', 401, headers={'Authorization':'Refresh '+login['refresh_token']})
    call(admin, 'DELETE', '/api/auth/sessions/' + digest(rotated['access_token']))
    stop(); start()
    assert not call(bearer(rotated['access_token']), 'GET', '/api/auth/status').json()['authenticated']
    call(replay, 'POST', '/api/auth/refresh', 401, headers={'Authorization':'Refresh '+rotated['refresh_token']})
    stop()
    legacy_access = 'previous-access-fixture'; legacy_refresh = 'previous-refresh-fixture'
    legacy_api = 'previous-api-fixture'
    doc = document()
    doc['root']['api_tokens'] = [{'hash':previous_digest(legacy_api), 'username':'test',
        'created_at':'17','scopes':[{'path':'/api/metadata','methods':['GET']}]}]
    doc['root']['session_tokens'] = [{'hash':previous_digest(legacy_access), 'username':'test',
        'refresh_token_hash':previous_digest(legacy_refresh), 'created_at':'17', 'expires_at':str(int(time.time())+60),
        'refresh_expires_at':str(int(time.time())+300), 'remember_me':'true', 'device_label':'Previous browser',
        'migration-field':9}]
    (directory / 'vibeshine_state.json').write_text(json.dumps(doc))
    start()
    old_api=bearer(legacy_api)
    call(old_api,'GET','/api/metadata')
    call(old_api,'GET','/api/config',401)
    previous_browser=bearer(legacy_access)
    assert call(previous_browser,'GET','/api/auth/status').json()['authenticated']
    previous_browser.headers['X-CSRF-Token']=call(previous_browser,'GET','/api/csrf-token').json()['csrf_token']
    call(previous_browser,'PATCH','/api/config',json={})
    sessions=call(previous_browser,'GET','/api/auth/sessions').json()['sessions']
    assert len(sessions)==1 and sessions[0]['current']
    call(previous_browser,'POST','/api/auth/logout')
    assert not call(previous_browser,'GET','/api/auth/status').json()['authenticated']
    stop()
    doc['root']['session_tokens'][0]['expires_at']=str(int(time.time())-10)
    (directory / 'vibeshine_state.json').write_text(json.dumps(doc))
    start()
    migrated = requests.Session(); migrated.verify = False
    migrated.cookies.set('__Host-apollo_session', legacy_access, domain='127.0.0.1', path='/', secure=True)
    migrated.cookies.set('__Host-apollo_refresh', legacy_refresh, domain='127.0.0.1', path='/', secure=True)
    assert not call(migrated, 'GET', '/api/auth/status').json()['authenticated']
    page = call(migrated, 'GET', '/library')
    assert 'Sign in</h1>' not in page.text and '<script' not in page.text
    assert call(migrated, 'GET', '/api/auth/status').json()['authenticated']
    row = document()['root']['session_tokens'][0]
    assert row['migration-field'] == 9 and row['device_label'] == 'Previous browser'
    assert row['refresh_expires_at'] == int(doc['root']['session_tokens'][0]['refresh_expires_at'])
    report = {'status':'pass', 'restarts':generation-1, 'checks':[
        'remembered login survives restart', 'only hashes saved', 'canonical username',
        'refresh rotation rejects old access and refresh tokens', 'absolute refresh lifetime',
        'revocation survives restart', 'previous string-valued session records import',
        'previous C++ password hash encoding', 'previous API token scopes preserved',
        'previous browser access, CSRF, current-session identity and logout',
        'expired access automatically renews through Rust HTML console', 'unknown fields preserved'],
        'fixture':str(directory)}
    (root / 'auth-session-restart.json').write_text(json.dumps(report, indent=2))
    print(json.dumps(report))
finally:
    stop()
    for output in outputs: output.close()
