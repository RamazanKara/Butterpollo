"""Real administration/auth contract checks against an isolated Rust instance."""
import argparse, hashlib, io, json, os, pathlib, re, time, uuid, zipfile
import requests, urllib3
urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
args = argparse.ArgumentParser()
args.add_argument('--url', default='https://127.0.0.1:48124')
args.add_argument('--username', default='test')
options = args.parse_args()
password = os.environ['BUTTERPOLLO_TEST_PASSWORD']
base = options.url.rstrip('/')
admin = requests.Session(); admin.verify = False; admin.auth = (options.username, password)
browser = requests.Session(); browser.verify = False
def request(client, method, path, code=200, **kw):
    response = client.request(method, base + path, timeout=15, **kw)
    assert response.status_code == code, (path, response.status_code)
    return response
assert request(browser, 'GET', '/api/auth/status').json()['authenticated'] is False
assert request(browser, 'GET', '/api/configLocale').json()['locale']
request(browser, 'GET', '/api/config', 401)
request(browser, 'POST', '/console/action', 400, data={'op':'login','_csrf':'missing','username':options.username,'password':password})
request(browser, 'POST', '/console/action', 401, data={'op':'theme','_csrf':'missing','theme':'dark'})
request(browser, 'POST', '/console/action', 400, data={'op':'/api/restart','_csrf':'missing'})
request(admin, 'PATCH', '/api/config', 403, json={}, headers={'Origin':'https://example.invalid'})
anonymous = request(browser, 'GET', '/api/csrf-token').json()['csrf_token']
request(browser, 'POST', '/api/auth/login', 400, json={'username':options.username,'password':password})
browser.headers['X-CSRF-Token'] = anonymous
login = request(browser, 'POST', '/api/auth/login', json={'username':options.username.upper(),'password':password}).json()
csrf = login['csrf_token']
browser.headers.pop('X-CSRF-Token')
request(browser, 'PATCH', '/api/config', 400, json={})
browser.headers['X-CSRF-Token'] = csrf
request(browser, 'PATCH', '/api/config', json={})
request(browser, 'POST', '/console/action', 400, data={'op':'theme','theme':'dark'})
request(browser, 'POST', '/console/action', 400, data={'op':'theme','_csrf':'wrong','theme':'dark'})
request(browser, 'POST', '/console/action', 403, data={'op':'theme','_csrf':csrf,'theme':'dark'},headers={'Origin':'https://example.invalid'})
theme=request(browser, 'POST', '/console/action', 303, data={'op':'theme','_csrf':csrf,'theme':'dark','_return':'https://example.invalid'},allow_redirects=False)
assert theme.headers['Location']=='/' and browser.cookies.get('butterpollo_theme')=='dark'
console=request(browser, 'GET', '/')
assert '<script' not in console.text and 'data-theme="dark"' in console.text
assert "default-src 'none'" in console.headers['Content-Security-Policy']
assert console.headers['Cache-Control']=='no-store'
request(browser, 'PATCH', '/api/config', 400, json={'capture':'wgc\nport = 9'})
assert request(browser, 'GET', '/api/metadata').json()['features']['rust_host'] is True
logs = request(browser, 'GET', '/api/logs')
assert logs.headers['content-type'].startswith('text/plain') and 'Butterpollo Rust host started' in logs.text
assert 'attachment' in request(browser, 'GET', '/api/logs/export').headers['content-disposition']
assert request(browser, 'GET', '/api/updates').json()['check_failed'] is False
layout = request(browser, 'GET', '/api/clients/display-layout').json()
assert layout['version'] == 1 and layout['capacity']['max'] == 4 and layout['nodes']
request(browser, 'PUT', '/api/clients/display-layout', 400, json={'version':1,'placements':{'unknown':{'anchor_kind':'client','anchor_id':'unknown','edge':'right','alignment':'center','gap_px':0}}})
assert request(browser, 'PUT', '/api/clients/display-layout', json=layout['layout']).json()['applies_on_next_activation']
for path in ['/api/health/crashdump','/api/rtss/status','/api/health/vulkan-hdr-layer']:
    request(browser, 'GET', path)
assert request(browser, 'POST', '/api/display/export_golden', json={}).json()['status']
assert request(browser, 'GET', '/api/display/golden_status?compare_current=1').json()['current_mismatch_reason'] == ''
assert request(browser, 'DELETE', '/api/display/golden').json()['deleted']
assert request(browser, 'GET', '/api/logs/export_crash/manifest').json()['parts'][0]['index'] == 1
bundle = request(browser, 'GET', '/api/logs/export_crash?part=1')
with zipfile.ZipFile(io.BytesIO(bundle.content)) as archive:
    assert archive.testzip() is None and 'diagnostics.json' in archive.namelist() and 'logs/butterpollo.log' in archive.namelist()
app_id = str(uuid.uuid4()); token_hash = None
try:
    app = {'name':'Rust web fixture','uuid':app_id,'cmd':'','image-path':'','migration-field':{'preserve':True}}
    assert request(browser, 'POST', '/api/apps', json=app).json()['uuid'] == app_id
    apps = request(browser, 'GET', '/api/apps').json()['apps']
    assert any(a.get('uuid') == app_id for a in apps)
    request(browser, 'POST', '/console/action', 303, data={'op':'app-save','_csrf':csrf,'uuid':app_id,'name':'Rust <script>alert(1)</script> fixture','cmd':'','working-dir':'','_return':'/library'},allow_redirects=False)
    saved=next(a for a in request(browser, 'GET', '/api/apps').json()['apps'] if a.get('uuid')==app_id)
    assert saved['migration-field']=={'preserve':True}
    library=request(browser, 'GET', '/library').text
    assert 'Rust &lt;script&gt;alert(1)&lt;/script&gt; fixture' in library and '<script' not in library
    cover = request(browser, 'GET', f'/api/apps/{app_id}/cover')
    assert cover.content.startswith(b'\x89PNG\r\n\x1a\n')
    if os.environ.get('BUTTERPOLLO_TEST_DIR'):
        output = pathlib.Path(os.environ['BUTTERPOLLO_TEST_DIR']) / 'web-app-output.log'
        if output.exists(): output.unlink()
        app.update({'cmd':'echo RUST_APP_OUTPUT','output':str(output),'auto-detach':False,'wait-all':False})
        request(browser, 'POST', '/api/apps', json=app)
        request(browser, 'POST', '/api/apps/launch', json={'uuid':app_id})
        deadline = time.monotonic() + 5
        while request(browser, 'GET', '/api/session/status').json()['appRunning'] and time.monotonic() < deadline: time.sleep(.1)
        assert request(browser, 'GET', '/api/session/status').json()['appRunning'] is False
        assert output.read_bytes().strip() == b'RUST_APP_OUTPUT'
    catalog = request(browser, 'GET', '/api/token/routes').json()['routes']
    assert any(r['path'] == '/api/apps' and 'GET' in r['methods'] for r in catalog)
    secret = request(browser, 'POST', '/api/token', json={'scopes':[{'path':'/api/apps','methods':['GET']}]}).json()['token']
    token_hash = hashlib.sha256(secret.encode()).hexdigest()
    scoped = requests.Session(); scoped.verify = False; scoped.headers['Authorization'] = 'Bearer ' + secret
    request(scoped, 'GET', '/api/apps')
    request(scoped, 'POST', '/api/apps', 401, json=app)
    request(scoped, 'GET', '/api/clients/list', 401)
    request(scoped, 'POST', '/api/token', 401, json={'scopes':[{'path':'/api/config','methods':['POST']}]})
    for op in ['app-save','config','theme']:
        request(scoped, 'POST', '/console/action', 401, data={'op':op,'_csrf':'token','name':'Forbidden','theme':'dark','path':'/api/apps','method':'GET'})
    tokens = request(browser, 'GET', '/api/tokens').json()['tokens']
    assert any(t['hash'] == token_hash for t in tokens)
    assert secret not in str(tokens)
    request(browser, 'DELETE', '/api/token/' + token_hash)
    request(scoped, 'GET', '/api/apps', 401)
    token_hash = None
    issued=request(browser, 'POST', '/console/action', data={'op':'token-create','_csrf':csrf,'scopes':json.dumps([{'path':'/api/apps','methods':['GET']}])},allow_redirects=False)
    secret=re.search(r'<pre class="secret">([a-f0-9]{64})</pre>',issued.text).group(1)
    token_hash=hashlib.sha256(secret.encode()).hexdigest()
    assert 'Location' not in issued.headers and issued.headers['Cache-Control']=='no-store'
    assert secret not in request(browser, 'GET', '/api-tokens').text
    request(browser, 'DELETE', '/api/token/'+token_hash)
    token_hash=None
    refresh = request(browser, 'POST', '/api/auth/refresh', json={'refresh_token':login['refresh_token']}).json()
    browser.headers['X-CSRF-Token'] = refresh['csrf_token']
    old = requests.Session(); old.verify=False; old.headers['Authorization']='Bearer '+login['access_token']
    request(old, 'GET', '/api/config', 401)
    sessions = request(browser, 'GET', '/api/auth/sessions').json()['sessions']
    current = next(s for s in sessions if s['current'])
    request(browser, 'DELETE', '/api/auth/sessions/' + current['id'])
    request(browser, 'GET', '/api/config', 401)
finally:
    request(admin, 'DELETE', '/api/apps/' + app_id)
    if token_hash: request(admin, 'DELETE', '/api/token/' + token_hash)
print('WEB API PASS: authentication/CSRF, Rust forms/scopes/escaping/secret handling, scoped tokens/revocation, app CRUD/covers, display layouts/baselines, maintenance health, support ZIP, logs')
