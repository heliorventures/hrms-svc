const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '../..');
const read = name => fs.readFileSync(path.join(root, name), 'utf8');
test('all source maintenance jobs have a dedicated source runner and no transport poll', () => {
  const maintenance = read('crates/kabipay-tenant-maintenance/src/main.rs');
  for (const job of ['process_due_separations', 'sweep_expired_attendance', 'process_due_surveys',
    'process_due_celebrations', 'process_due_performance_cycles', 'process_due_performance_stage_deadlines',
    'sweep_expired_company_upload_stages', 'announcement_video::sweep_expired', 'process_private_file_cleanup_tasks']) {
    assert.ok(maintenance.includes(job), `missing source job ${job}`);
  }
  assert.match(maintenance, /TenantBusinessClock::load/);
  assert.match(maintenance, /state\.allows\("EMPLOYEE"\)/);
  assert.match(maintenance, /state\.allows\("ATTENDANCE"\)/);
  assert.match(maintenance, /bound_tenant_id/);
  assert.match(maintenance, /requires a bound company source/);
  assert.doesNotMatch(maintenance, /outbox_event|process_tenant_outbox|OUTBOX_WEBHOOK/);
  const legacyPath = 'crates/kabipay-outbox-worker/src/main.rs';
  const legacy = fs.existsSync(path.join(root, legacyPath)) ? read(legacyPath) : '';
  assert.doesNotMatch(legacy, /process_due_separations|process_due_surveys|process_due_celebrations|process_due_performance_cycles|process_private_file_cleanup_tasks/);
});

test('Finance uses source-owned immutable delivery and no poller starts', () => {
  const producer = read('crates/kabipay-employee/src/services/finance_operation.rs');
  assert.match(producer, /private_events::enqueue_delivery/);
  assert.doesNotMatch(producer, /INSERT INTO employee_finance_outbox/);
  const main = read('crates/kabipay-employee/src/main.rs');
  assert.match(main, /http_private_delivery::routes/);
  assert.doesNotMatch(main, /finance_worker::run/);
  const adapter = read('crates/kabipay-employee/src/services/private_delivery.rs');
  for (const guard of ['source_generation', 'destination_version', 'intent_hash', 'lease_generation',
    'verify_request', 'decrypt_frames', 'finance_transport::send', 'acknowledgement_id']) {
    assert.ok(adapter.includes(guard), `missing adapter guard ${guard}`);
  }
});
