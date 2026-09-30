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
artifact=pathlib.Path(sys.argv[1]);client_exe=artifact/'moonlight-client.exe';codec=sys.argv[2] if len(sys.argv)>2 else 'h264'
web='https://127.0.0.1:48124';http='http://127.0.0.1:48123';https='https://127.0.0.1:48118'
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
r=session.post(web+'/api/clients/update',json={'uuid':paired['uuid'],'perm':0x071f1f00},timeout=10);r.raise_for_status()
client=requests.Session();client.verify=False;client.cert=(str(artifact/'client.pem'),str(artifact/'client-key.pem'))
info=ET.fromstring(client.get(https+'/serverinfo',timeout=10).text);assert info.findtext('PairStatus')=='1'
apps=ET.fromstring(client.get(https+'/applist',timeout=10).text);app=apps.find('App');assert app is not None
launch=ET.fromstring(client.get(https+'/launch',params={'appid':app.findtext('ID'),'rikey':bytes(range(16)).hex(),'rikeyid':'123','corever':'1'},timeout=10).text);assert launch.attrib['status_code']=='200',ET.tostring(launch)
url=launch.findtext('sessionUrl0');print('LAUNCH',url,flush=True)
env=os.environ.copy();env['PATH']=str(artifact/'target/debug')+';C:\\msys64\\ucrt64\\bin;'+env['PATH']
try:
    result=subprocess.run([str(client_exe),url,codec],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
    print(result.stdout,flush=True);(artifact/'moonlight-interop.log').write_text(result.stdout,encoding='utf-8');assert result.returncode==0
except subprocess.TimeoutExpired as error:
    output=error.stdout or b''
    if isinstance(output,bytes):output=output.decode('utf-8',errors='replace')
    print(output,flush=True);(artifact/'moonlight-interop.log').write_text(output,encoding='utf-8')
    raise
finally:
    client.get(https+'/cancel',timeout=10)
    session.post(web+'/api/clients/unpair',json={'uuid':paired['uuid']},timeout=10).raise_for_status()
print('INTEROPERABILITY PASS',flush=True)
