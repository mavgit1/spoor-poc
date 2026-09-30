// Every DNS record of every domain in a Hostpoint account. Read-only.
//
//   spoor exec hostpoint examples/hostpoint/export-dns.js --timeout 1800 > dns.json
//   spoor exec hostpoint examples/hostpoint/export-dns.js --args '{"only":["example.ch"]}'
//
// Runs on the site url (/customer/Index), so the CSRF token is in the page.
// No pacing here: Spoor spaces every request by the site's min_gap_ms.
// Returns { zones: [{domain, domainId, status, records: [...]}], errors: [...] }.

const csrf = document.querySelector('input[name="csrf_token"]')?.value;
if (!csrf) throw new Error('no CSRF token on the page — logged out? run `spoor open hostpoint`');

const json = { 'X-CSRF-Token': csrf, accept: 'application/json' };

// 1. Domain list (paged by 100). The DNS editor is not available for all of them.
const domains = [];
for (let offset = 0; ; offset += 100) {
  const r = await fetch(
    `/api/customer/domains?nameContains=&resolveNameservers=false&offset=${offset}&limit=100`,
    { headers: json });
  if (!r.ok) throw new Error(`domain list HTTP ${r.status}`);
  const page = await r.json();
  domains.push(...page.data);
  if (!page.data.length || (!page.countInfo.hasMore && domains.length >= page.countInfo.count)) break;
}
let targets = domains.filter(d => d.dnsEditorAccessible);
if (args.only) targets = targets.filter(d => args.only.includes(d.domainNameAscii));
console.log(`${domains.length} domains, ${targets.length} with DNS editor`);

// 2. Per domain: the DNS page carries the numeric domainId in a hidden input;
//    records come from a POST that only reads.
const zones = [], errors = [];
for (const [i, d] of targets.entries()) {
  const name = d.domainNameAscii;
  const page = `/customer/Domains/Dns/Edit?name=${encodeURIComponent(name)}`;
  try {
    const html = await (await fetch(page)).text();
    const id = html.match(/name="domainId"\s+value="(\d+)"/)?.[1];
    if (!id) throw new Error('domainId not found on DNS page');
    const r = await fetch(page, {
      method: 'POST',
      headers: {
        'X-CSRF-Token': csrf,
        'X-Requested-With': 'XMLHttpRequest',
        Accept: 'application/json, text/javascript, */*; q=0.01',
        'Content-Type': 'application/x-www-form-urlencoded; charset=UTF-8',
      },
      body: `_action_get_records=1&id=${id}&domainId=${id}`,
    });
    if (!r.ok) throw new Error(`records HTTP ${r.status}`);
    const records = ((await r.json()).records || []).map(rec => ({
      name: rec.full_name,
      subpart: rec.subpart,
      type: rec.type,
      ttl: rec.ttl,
      prio: rec.prio,
      content: rec.content,
      managed_by_system: rec.managed_by_system,
      hidden_from_customer: rec._is_hidden_from_customer,
    }));
    zones.push({ domain: name, domainId: id, status: d.status, records });
    console.log(`[${i + 1}/${targets.length}] ${name}: ${records.length} records`);
  } catch (e) {
    errors.push({ domain: name, error: String(e.message || e) });
    console.warn(`[${i + 1}/${targets.length}] ${name}: ${e}`);
  }
}
return { zones, errors };
