// Run against an isolated host. Requires Playwright and its Chromium runtime.
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const base = process.env.BUTTERPOLLO_TEST_URL || 'https://127.0.0.1:48124';
const output = process.env.BUTTERPOLLO_TEST_ARTIFACTS;
const password = process.env.BUTTERPOLLO_TEST_PASSWORD;
assert(output && password, 'Set test artifact directory and test password');
(async () => {
  const browser = await chromium.launch({headless:true, ...(process.env.CHROMIUM_EXECUTABLE ? {executablePath:process.env.CHROMIUM_EXECUTABLE} : {})});
  const context = await browser.newContext({ignoreHTTPSErrors:true, javaScriptEnabled:false, viewport:{width:1440,height:900}});
  const page = await context.newPage();
  const errors = [], failed = [], scripts = [], results = [];
  let uuid, localeRestore;
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if(message.type()==='error') errors.push(message.text()); });
  page.on('response', response => { if(response.status()>=400) failed.push([response.url(),response.status()]); });
  page.on('request', request => { if(request.resourceType()==='script') scripts.push(request.url()); });
  async function api(method, route, data) {
    const csrf = await (await context.request.get(base+'/api/csrf-token')).json();
    const response = await context.request.fetch(base+route, {method, data, headers:{'X-CSRF-Token':csrf.csrf_token}});
    assert(response.ok(), route+' '+response.status()); return response.json();
  }
  function editor() { return page.locator('form').filter({has:page.locator('input[name=op][value=app-save]')}); }
  function app() { return page.locator('section.app').filter({has:page.getByRole('heading',{name:'Rust browser <script> fixture',exact:true})}); }
  try {
    await page.goto(base+'/'); assert(page.url().endsWith('/login'));
    await page.getByLabel('Username',{exact:true}).fill('test');
    await page.getByLabel('Password',{exact:true}).fill(password);
    await page.getByLabel('Remember this device',{exact:true}).check();
    await page.getByRole('button',{name:'Sign in',exact:true}).click();
    assert.equal(page.url(),base+'/');
    assert((await page.title()).includes('Rubylight'));
    assert(await page.getByRole('heading',{name:'Connection checklist',exact:true}).isVisible());
    const metadata = await api('GET','/api/metadata');
    results.push({flow:'first_stream',status:metadata.paired_devices===0?'tested':'not_applicable_with_paired_devices'});
    if(metadata.paired_devices===0) {
      const firstStream = page.locator('section.card').filter({has:page.getByRole('heading',{name:'Your first stream',exact:true})});
      assert(await firstStream.isVisible());
      assert.equal(await firstStream.locator('code').innerText(),metadata.pc_address);
      const hostPort=Number(new URL(base).port)-1;
      if(hostPort!==47989) assert(metadata.pc_address.endsWith(':'+hostPort),'Manual Add PC must include the custom Moonlight port');
      assert((await firstStream.innerText()).includes('PIN shown by Moonlight'));
      await page.screenshot({path:path.join(output,'rust-ui-first-stream-desktop.png'),fullPage:true});
      await page.setViewportSize({width:390,height:844});
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
      await page.screenshot({path:path.join(output,'rust-ui-first-stream-mobile.png'),fullPage:true});
      await page.setViewportSize({width:1440,height:900});
    }
    const liveResponse = await context.request.get(base+'/?live=1');
    assert.equal(liveResponse.headers().refresh,'5; url=/?live=1');
    await page.getByRole('link',{name:'Update every five seconds',exact:true}).click();
    assert.equal(page.url(),base+'/?live=1');
    await page.getByRole('link',{name:'Pause updates',exact:true}).click();
    assert.equal(page.url(),base+'/');
    assert.equal((await context.request.get(base+'/')).headers().refresh,undefined);
    await page.locator('.quick-links').getByRole('link',{name:/^Pair a device/}).click();
    assert.equal(page.url(),base+'/devices');
    assert((await page.locator('main').innerText()).includes('Moonlight shows a four-digit PIN'));
    await page.getByRole('link',{name:'Check for a pairing request',exact:true}).click();
    assert.equal(page.url(),base+'/devices');
    for(const route of ['/','/library','/devices','/logs','/settings','/integrations','/api-tokens','/maintenance']) {
      await page.goto(base+route); assert.equal(await page.locator('script').count(),0);
      assert.deepEqual(await page.locator('[role=alert]').allTextContents(),[]);
      results.push({route, heading:await page.locator('h1').innerText()});
    }
    await page.goto(base+'/library');
    await editor().getByLabel('Application name',{exact:true}).fill('Rust browser <script> fixture');
    await editor().getByText('Preparation commands and advanced options',{exact:true}).click();
    await editor().getByLabel('Application options (JSON)',{exact:true}).fill(JSON.stringify({'migration-field':{preserve:true}}));
    await editor().getByText('Application TrueHDR',{exact:true}).click();
    await editor().getByLabel('TrueHDR override',{exact:true}).selectOption('true');
    await editor().getByLabel('TrueHDR contrast override',{exact:true}).fill('14');
    await editor().getByLabel('TrueHDR peak brightness override (nits)',{exact:true}).fill('1500');
    await editor().getByRole('button',{name:'Save application',exact:true}).click();
    let saved = (await api('GET','/api/apps')).apps.find(a=>a.name==='Rust browser <script> fixture');
    uuid = saved.uuid; assert.equal(saved['rtx-hdr'],'true'); assert.equal(saved['rtx-hdr-contrast'],'14');
    assert.deepEqual(saved['migration-field'],{preserve:true});
    await app().getByRole('button',{name:'Move up',exact:true}).click();
    assert.equal((await api('GET','/api/apps')).apps[0].uuid,uuid);
    // Import an older nested override and verify the typed editor materializes it.
    saved['config-overrides'] = {rtx_hdr_contrast:27,'migration-tuning':9};
    await api('POST','/api/apps',saved);
    await page.goto(base+'/library?edit='+encodeURIComponent(uuid));
    await editor().getByText('Application TrueHDR',{exact:true}).click();
    assert.equal(await editor().getByLabel('TrueHDR contrast override',{exact:true}).inputValue(),'27');
    for (const label of ['SDR brightness override','TrueHDR saturation override','TrueHDR middle gray override']) {
      assert.equal(await editor().getByLabel(label,{exact:true}).inputValue(),'');
    }
    const live = page.getByLabel('TrueHDR settings (JSON)',{exact:true});
    assert.equal(JSON.parse(await live.inputValue()).rtx_hdr,'true');
    await editor().getByLabel('TrueHDR contrast override',{exact:true}).fill('19');
    await page.setViewportSize({width:390,height:844});
    await editor().scrollIntoViewIfNeeded();
    await page.screenshot({path:path.join(output,'rust-ui-application-mobile.png')});
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
    await editor().getByRole('button',{name:'Save application',exact:true}).click();
    saved = (await api('GET','/api/apps')).apps.find(a=>a.uuid===uuid);
    assert.equal(saved['rtx-hdr-contrast'],'19');
    assert(!Object.hasOwn(saved['config-overrides'],'rtx_hdr_contrast'));
    assert.equal(saved['config-overrides']['migration-tuning'],9);
    await page.goto(base+'/library?edit='+encodeURIComponent(uuid));
    await editor().getByText('Application TrueHDR',{exact:true}).click();
    await editor().getByLabel('TrueHDR override',{exact:true}).selectOption('');
    await editor().getByLabel('TrueHDR contrast override',{exact:true}).fill('');
    await editor().getByLabel('TrueHDR peak brightness override (nits)',{exact:true}).fill('');
    await editor().getByRole('button',{name:'Save application',exact:true}).click();
    saved = (await api('GET','/api/apps')).apps.find(a=>a.uuid===uuid);
    assert.equal(saved['rtx-hdr'],null); assert.equal(saved['rtx-hdr-contrast'],null);
    assert.deepEqual(saved['migration-field'],{preserve:true});
    localeRestore = (await api('GET','/api/config')).locale ?? null;
    const german = JSON.parse(fs.readFileSync(path.join(__dirname,'../assets/locale/de.json'),'utf8'));
    await api('POST','/api/apps',{...saved,name:'Settings'});
    await api('PATCH','/api/config',{locale:'de'});
    await page.goto(base+'/library');
    assert.equal(await page.locator('html').getAttribute('lang'),'de');
    assert.equal(await page.locator('h1').innerText(),german.navbar.applications);
    assert.equal(await page.locator('section.app h2').filter({hasText:/^Settings$/}).count(),1);
    await page.goto(base+'/settings');
    assert.equal(await page.locator('h1').innerText(),german.navbar.configuration);
    assert.equal(await page.getByLabel(german.config.locale,{exact:true}).inputValue(),'de');
    await page.locator('details').filter({has:page.locator('[name=cfg_locale]')}).locator('summary').click();
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
    await page.screenshot({path:path.join(output,'rust-ui-german-mobile.png')});
    await api('PATCH','/api/config',{locale:'zh-TW'});
    await page.goto(base+'/settings');
    assert.equal(await page.locator('html').getAttribute('lang'),'zh-TW');
    assert.equal(await page.locator('[name=cfg_locale]').inputValue(),'zh-TW');
    await api('PATCH','/api/config',{locale:localeRestore}); localeRestore = undefined;
    await api('POST','/api/apps',saved);
    await page.goto(base+'/library');
    await app().getByRole('button',{name:'Remove',exact:true}).click(); uuid = undefined;
    await page.setViewportSize({width:1440,height:900}); await page.goto(base+'/');
    await page.getByLabel('Appearance',{exact:true}).selectOption('dark');
    await page.getByRole('button',{name:'Apply',exact:true}).click();
    assert.equal(await page.locator('html').getAttribute('data-theme'),'dark');
    await page.screenshot({path:path.join(output,'rust-ui-dark.png')});
    await page.getByLabel('Appearance',{exact:true}).selectOption('light'); await page.getByRole('button',{name:'Apply',exact:true}).click();
    await page.screenshot({path:path.join(output,'rust-ui-overview.png')});
    await page.setViewportSize({width:390,height:844}); await page.screenshot({path:path.join(output,'rust-ui-mobile.png')});
    await page.getByRole('button',{name:'Sign out',exact:true}).click(); assert(page.url().endsWith('/login'));
    assert.deepEqual(errors,[]); assert.deepEqual(failed,[]); assert.deepEqual(scripts,[]);
    const report = {status:'pass',environment:'Chromium, JavaScript disabled, 1440x900 and 390x844',results,
      checks:['remembered sign-in','eight administration pages','escaped application CRUD','ordering',
        'connection checklist and host network address','first-stream instructions on desktop/mobile',
        'optional live refresh and pause','Moonlight pairing instructions and request reload',
        'typed TrueHDR and previous nested overrides','unknown-field preservation','reset to inherited settings',
        'live editor projects typed settings','mobile form without overflow','German and traditional Chinese locales preserve data',
        'theme persistence','sign-out'],errors,failed,scripts};
    fs.writeFileSync(path.join(output,'rust-ui-qa.json'),JSON.stringify(report,null,2)); console.log(JSON.stringify(report));
  } finally {
    if(localeRestore !== undefined) { try { await api('PATCH','/api/config',{locale:localeRestore}); } catch {} }
    if(uuid) { try { await api('DELETE','/api/apps/'+uuid); } catch {} }
    await browser.close();
  }
})().catch(error=>{console.error(error);process.exitCode=1});
