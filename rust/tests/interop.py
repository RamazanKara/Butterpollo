"""Independent pairing + Moonlight client probe. Uses an isolated host configuration."""
import concurrent.futures, hashlib, os, pathlib, subprocess, sys, time
import xml.etree.ElementTree as ET
from datetime import datetime, timedelta, timezone
import requests, urllib3
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa,padding
from cryptography.hazmat.primitives.ciphers import Cipher,algorithms,modes
from cryptography.x509.oid import NameOID
urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
artifact=pathlib.Path(sys.argv[1]);client_exe=pathlib.Path(os.environ.get('BUTTERPOLLO_TEST_CLIENT_EXE',artifact/('moonlight-client.exe' if os.name=='nt' else 'moonlight-client')));codec=sys.argv[2] if len(sys.argv)>2 else 'h264'
port=int(os.environ.get('BUTTERPOLLO_TEST_PORT','48123'))
host=os.environ.get('BUTTERPOLLO_TEST_HOST','127.0.0.1')
address=f'[{host}]' if ':' in host else host
web=f'https://{address}:{port+1}';http=f'http://{address}:{port}';https=f'https://{address}:{port-5}'
session=requests.Session();session.verify=False
login=session.post(web+'/api/auth/login',json={'username':'test','password':'rust-smoke-only'},timeout=10);login.raise_for_status();csrf=login.json()['csrf_token'];session.headers['X-CSRF-Token']=csrf
key=rsa.generate_private_key(public_exponent=65537,key_size=2048)
subject=x509.Name([x509.NameAttribute(NameOID.COMMON_NAME,'Rust interoperability probe')])
cert=x509.CertificateBuilder().subject_name(subject).issuer_name(subject).public_key(key.public_key()).serial_number(x509.random_serial_number()).not_valid_before(datetime.now(timezone.utc)-timedelta(days=1)).not_valid_after(datetime.now(timezone.utc)+timedelta(days=30)).sign(key,hashes.SHA256())
cert_pem=cert.public_bytes(serialization.Encoding.PEM);key_pem=key.private_bytes(serialization.Encoding.PEM,serialization.PrivateFormat.PKCS8,serialization.NoEncryption())
(artifact/'client.pem').write_bytes(cert_pem);(artifact/'client-key.pem').write_bytes(key_pem)
salt=os.urandom(16);pin='1234';aes=hashlib.sha256(salt+pin.encode()).digest()[:16]
def ecb(data,encrypt=True):
    c=Cipher(algorithms.AES(aes),modes.ECB());op=c.encryptor() if encrypt else c.decryptor();return op.update(data)+op.finalize()
uid='rust-probe-'+os.urandom(4).hex()
def pair(args):
    r=requests.get(http+'/pair',params={'uniqueid':uid,**args},timeout=20);r.raise_for_status();root=ET.fromstring(r.text);assert root.attrib['status_code']=='200',r.text;assert root.findtext('paired')=='1',r.text;return root
with concurrent.futures.ThreadPoolExecutor() as executor:
    future=executor.submit(pair,{'phrase':'getservercert','salt':salt.hex(),'clientcert':cert_pem.hex(),'devicename':uid})
    time.sleep(.3)
    r=session.post(web+'/api/pin',json={'pin':pin,'name':uid,'uniqueid':uid},timeout=10);r.raise_for_status()
    root=future.result(timeout=20)
server=x509.load_pem_x509_certificate(bytes.fromhex(root.findtext('plaincert')))
challenge=os.urandom(16)
root=pair({'clientchallenge':ecb(challenge).hex()});response=ecb(bytes.fromhex(root.findtext('challengeresponse')),False)
secret=os.urandom(16);proof=hashlib.sha256(response[32:]+cert.signature+secret).digest()
root=pair({'serverchallengeresp':ecb(proof).hex()});server_proof=bytes.fromhex(root.findtext('pairingsecret'))
assert response[:32]==hashlib.sha256(challenge+server.signature+server_proof[:16]).digest()
server.public_key().verify(server_proof[16:],server_proof[:16],padding.PKCS1v15(),hashes.SHA256())
pair({'clientpairingsecret':(secret+key.sign(secret,padding.PKCS1v15(),hashes.SHA256())).hex()})
print('PAIRING independent RSA/AES proof verified',flush=True)
# New clients after the first one deliberately receive view-only permissions.
# The administrator explicitly grants this fixture permission to launch.
clients=session.get(web+'/api/clients/list',timeout=10).json()['clients']
paired=next(c for c in clients if c['name']==uid)
client=requests.Session();client.verify=False;client.cert=(str(artifact/'client.pem'),str(artifact/'client-key.pem'))
# File upload permission must not grant global stream termination. Check both
# cancellation and the legacy termination tile before granting launch rights.
session.post(web+'/api/clients/update',json={'uuid':paired['uuid'],'perm':(1<<24)|(1<<18)},timeout=10).raise_for_status()
denied=ET.fromstring(client.get(https+'/cancel',timeout=10).text)
assert denied.attrib['status_code']=='401',ET.tostring(denied)
denied=ET.fromstring(client.get(https+'/launch',params={'appid':'2147483504'},timeout=10).text)
assert denied.attrib['status_code']=='401',ET.tostring(denied)
visible=ET.fromstring(client.get(https+'/applist',timeout=10).text)
assert all(app.findtext('AppTitle')!='Terminate' for app in visible.findall('App'))
print('PERMISSIONS file upload cannot terminate streams',flush=True)
settings={'uuid':paired['uuid'],'perm':0x071f1f00}
hooks_path=artifact/'client-hooks.txt' if os.environ.get('BUTTERPOLLO_TEST_CLIENT_HOOKS')=='1' else None
if hooks_path:
    hooks_path.unlink(missing_ok=True)
    settings.update({'do':[{'cmd':f'echo connected-$(SUNSHINE_CLIENT_NAME)>"{hooks_path}"'}],
                     'undo':[{'cmd':f'echo disconnected-$(SUNSHINE_CLIENT_NAME)>>"{hooks_path}"'}]})
r=session.post(web+'/api/clients/update',json=settings,timeout=10);r.raise_for_status()
info=ET.fromstring(client.get(https+'/serverinfo',timeout=10).text);assert info.findtext('PairStatus')=='1'
if codec.startswith('pyrowave'):
    assert int(info.findtext('ServerCodecModeSupport','0')) & 0x00800000
    assert info.findtext('PyroWaveBandwidthProbeBytes')=='33554432'
    probe=client.get(https+'/pyrowave-bandwidth-probe',timeout=10)
    probe.raise_for_status();assert len(probe.content)==33554432
    assert probe.headers['Cache-Control']=='no-store'
    denied=requests.get(https+'/pyrowave-bandwidth-probe',verify=False,timeout=10)
    assert denied.status_code==401
    print('PYROWAVE paired bandwidth calibration and access control verified',flush=True)
apps=ET.fromstring(client.get(https+'/applist',timeout=10).text);app=apps.find('App');assert app is not None
key_id = '-2147483525' if os.environ.get('BUTTERPOLLO_TEST_SIGNED_KEY_ID') == '1' else '123'
launch_args={'appid':app.findtext('ID'),'rikey':bytes(range(16)).hex(),'rikeyid':key_id,'corever':'1'}
if os.environ.get('BUTTERPOLLO_TEST_MATCH_DISPLAY')=='1':
    launch_args.update(mode='x'.join(sys.argv[3:6]),hdrMode='1' if codec.endswith('-hdr') else '0')
limiter_lifecycle = None
if os.environ.get('BUTTERPOLLO_TEST_RTSS_PROFILE'):
    from rtss_lifecycle import Lifecycle
    limiter_lifecycle = Lifecycle(os.environ['BUTTERPOLLO_TEST_RTSS_PROFILE'], session, http, web, artifact)
launch=ET.fromstring(client.get(https+'/launch',params=launch_args,timeout=10).text);assert launch.attrib['status_code']=='200',ET.tostring(launch)
if limiter_lifecycle: limiter_lifecycle.launched()
url=launch.findtext('sessionUrl0');print('LAUNCH',url,flush=True)
env=os.environ.copy()
if os.name=='nt':env['PATH']=str(artifact/'target/debug')+';C:\\msys64\\ucrt64\\bin;'+env['PATH']
try:
    duration=int(sys.argv[6]) if len(sys.argv)>6 else 0
    command=[str(client_exe),url,codec,*sys.argv[3:]]
    samples=[];started=time.monotonic()
    process=subprocess.Popen(command,env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    with concurrent.futures.ThreadPoolExecutor() as executor:
        output_future=executor.submit(process.communicate)
        while not output_future.done():
            if time.monotonic()-started>max(30,duration+30):
                process.kill()
                output=output_future.result()[0]
                raise subprocess.TimeoutExpired(command,max(30,duration+30),output=output)
            stats=session.get(web+'/api/rtsp/sessions',timeout=10);stats.raise_for_status()
            samples.append({'elapsed_seconds':time.monotonic()-started,**stats.json()})
            time.sleep(.5)
        output=output_future.result()[0]
    result=subprocess.CompletedProcess(command,process.returncode,output)
    stream_samples=[s for sample in samples for s in sample.get('sessions',[]) if s['uuid']==paired['uuid'] and s['uptime_seconds']>=2]
    steady_fps=None
    if len(stream_samples)>1:
        first,last=stream_samples[0],stream_samples[-1]
        steady_fps=(last['frames_sent']-first['frames_sent'])/(last['uptime_seconds']-first['uptime_seconds'])
    threads=sys.argv[8] if len(sys.argv)>8 else '1'
    report={'codec':codec,'client_arguments':sys.argv[3:],'decoder_threads':int(threads),'host_steady_fps':steady_fps,'samples':samples}
    import json
    width=sys.argv[3] if len(sys.argv)>3 else '640';height=sys.argv[4] if len(sys.argv)>4 else '480';fps=sys.argv[5] if len(sys.argv)>5 else '30'
    (artifact/f'stream-{codec}-{width}x{height}-{fps}-threads{threads}.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
    print('HOST_STEADY_FPS',steady_fps,flush=True)
    print(result.stdout,flush=True);(artifact/'moonlight-interop.log').write_text(result.stdout,encoding='utf-8');assert result.returncode==0
    if limiter_lifecycle: limiter_lifecycle.reconnect_and_expire(client, https, launch_args, command, env)
except subprocess.TimeoutExpired as error:
    output=error.stdout or b''
    if isinstance(output,bytes):output=output.decode('utf-8',errors='replace')
    print(output,flush=True);(artifact/'moonlight-interop.log').write_text(output,encoding='utf-8')
    raise
finally:
    client.get(https+'/cancel',timeout=10)
    session.post(web+'/api/clients/unpair',json={'uuid':paired['uuid']},timeout=10).raise_for_status()
if hooks_path:
    deadline=time.monotonic()+5
    while time.monotonic()<deadline:
        lines=hooks_path.read_text().splitlines() if hooks_path.exists() else []
        if lines==['connected-'+uid,'disconnected-'+uid]:break
        time.sleep(.05)
    assert lines==['connected-'+uid,'disconnected-'+uid],lines
    print('CLIENT CONNECT/DISCONNECT HOOKS PASS',flush=True)
print('INTEROPERABILITY PASS',flush=True)
