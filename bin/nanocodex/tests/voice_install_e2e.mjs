// Public voice-runtime install journey. Requires a real nanocodex CLI and
// nanocodex-hand built from one checkout, plus a real nanocodex-voice-<target>
// archive (a published release asset or scripts/build-voice-release.py output).
// The updater runs against an isolated HOME and NANOCODEX_DIR with a synthetic
// account; it never passes --restart-hand, bootstraps a service, or opens audio
// devices. Every packaged program must be installed executable and launch: the
// helper prints its build commit, and the macOS recorder is started without
// arguments so it prints usage and exits before requesting microphone access.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

assert.ok(['darwin', 'linux'].includes(process.platform), 'run on macOS or Linux');
const [suppliedCli, suppliedHand, suppliedArchive, outputArg] = process.argv.slice(2);
assert.ok(suppliedCli && suppliedHand && suppliedArchive,
  'usage: node bin/nanocodex/tests/voice_install_e2e.mjs CLI HAND VOICE_ARCHIVE [OUTPUT_DIR]');
const output = resolve(outputArg ?? 'output/voice-install-e2e');
mkdirSync(output, { recursive: true });
const fixture = mkdtempSync(join(tmpdir(), 'nanocodex voice install '));
const store = join(fixture, 'install');
const home = join(fixture, 'home');
const runner = join(fixture, 'runner', 'nanocodex');
const cli = join(fixture, 'pair', 'nanocodex');
const hand = join(fixture, 'pair', 'nanocodex-hand');
const archive = join(fixture, 'archive', 'nanocodex-voice.tar.gz');
const expanded = join(fixture, 'expanded');
const account = join(home, 'synthetic-account.json');
for (const path of [store, home, expanded, join(fixture, 'runner'), join(fixture, 'pair'), join(fixture, 'archive')]) {
  mkdirSync(path, { recursive: true });
}
for (const [source, target, mode] of [[suppliedCli, runner, 0o755], [suppliedCli, cli, 0o755],
  [suppliedHand, hand, 0o755], [suppliedArchive, archive, 0o644]]) {
  copyFileSync(resolve(source), target);
  chmodSync(target, mode);
}
writeFileSync(join(store, 'automatic-updates-disabled'), '');
// Deliberately invalid synthetic account: no saved credential is ever read.
writeFileSync(account, JSON.stringify({ fixture: true, access_token: 'synthetic-not-a-real-credential' }), { mode: 0o600 });
const env = {
  PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: join(home, '.config'),
  NANOCODEX_DIR: store, NANOCODEX_ACCOUNT_FILE: account, NO_COLOR: '1',
  TMPDIR: process.env.TMPDIR ?? tmpdir(),
};
const trace = [];
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const summary = { verdict: 'FAILED', platform: process.platform, fixture, archive: digest(readFileSync(archive)), checks: [],
  limitations: ['Launches packaged programs without opening audio devices; live microphone/speaker conversations are not covered.'] };
function save() {
  writeFileSync(join(output, 'transcript.log'), trace.join('\n\n') + '\n');
  writeFileSync(join(output, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
}
function run(program, args, options = {}) {
  const r = spawnSync(program, args, { cwd: fixture, env: { ...env, ...options.env }, encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'], timeout: options.timeout ?? 600_000, maxBuffer: 16 * 1024 * 1024 });
  trace.push('$ ' + program + ' ' + args.join(' ') + '\nexpected exit: ' + (options.expect ?? 0) +
    '\nobserved exit: ' + r.status + '; signal: ' + r.signal + '; error: ' + (r.error?.message ?? 'none') +
    '\nstdout:\n' + (r.stdout ?? '') + '\nstderr:\n' + (r.stderr ?? ''));
  save();
  assert.equal(r.error, undefined, 'child execution error');
  assert.equal(r.status, options.expect ?? 0, program + ' exit status');
  return r;
}
function pass(check) {
  summary.checks.push(check);
  trace.push('PASS ' + check);
  save();
}
function livePid() {
  if (process.platform !== 'darwin') return null;
  // Read-only: the user's real login Hand must never be restarted by this test.
  const r = spawnSync('launchctl', ['print', 'gui/' + process.getuid() + '/com.nanocodex.hand'], { encoding: 'utf8' });
  return r.status === 0 ? (r.stdout.match(/^\s*pid = (\d+)/m)?.[1] ?? 'not running') : 'not loaded';
}
// The updater names a local pair by the length-prefixed bytes it installs.
function localKey(paths) {
  const h = createHash('sha256');
  for (const p of paths) {
    const bytes = readFileSync(p);
    const size = Buffer.alloc(8);
    size.writeBigUInt64LE(BigInt(bytes.length));
    h.update(size).update(bytes);
  }
  return 'local-' + h.digest('hex').slice(0, 12);
}

const pidBefore = livePid();
trace.push('live Hand before (read-only launchctl print): ' + pidBefore + '\nfixture: ' + fixture);
let plist;
let plistBefore;
try {
  if (process.platform === 'darwin') {
    // Not bootstrapped. Native status reads this fixture plist and, if loaded,
    // the user's real login Hand (read-only), so the pair is either staged or
    // activated without a Hand switch. No restart is ever requested.
    const escape = x => x.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
    plist = join(home, 'Library/LaunchAgents/com.nanocodex.hand.plist');
    mkdirSync(join(home, 'Library/LaunchAgents'), { recursive: true });
    plistBefore = '<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>com.nanocodex.hand</string><key>ProgramArguments</key><array><string>' + escape(hand) + '</string><string>hand</string></array></dict></plist>';
    writeFileSync(plist, plistBefore);
  }
  run('tar', ['-xzf', archive, '-C', expanded]);
  const listing = run('tar', ['-tzf', archive]).stdout.split('\n').filter(Boolean);
  const programs = listing.filter(name => name.startsWith('nanocodex-resources/voice/bin/'));
  assert.ok(programs.some(name => name.endsWith('/nanocodex-voice-host') || name.endsWith('/nanocodex-voice-host.exe')),
    'archive must contain the voice helper');
  if (process.platform === 'darwin') {
    assert.ok(programs.includes('nanocodex-resources/voice/bin/nanocodex-voice-recorder'),
      'macOS archive must contain the microphone recorder');
  }
  const install = ['update', '--path', cli, '--hand-binary', hand, '--voice-archive', archive];
  const key = localKey([cli, hand, archive]);
  const version = join(store, 'versions', key);
  function verifyPrograms(label) {
    for (const name of programs) {
      const installed = join(version, name);
      const mode = statSync(installed).mode & 0o777;
      assert.equal(digest(readFileSync(installed)), digest(readFileSync(join(expanded, name))), label + ': ' + name + ' bytes');
      assert.notEqual(mode & 0o111, 0, label + ': ' + name + ' must be executable (mode ' + mode.toString(8) + ')');
    }
    const helper = run(join(version, 'nanocodex-resources/voice/bin/nanocodex-voice-host'), ['--build-commit']);
    assert.ok(helper.stdout.trim(), 'helper reports its build commit');
    // The macOS static engine receipt names its helper build; GStreamer
    // runtimes carry the sealed public-release receipt.
    const receipt = JSON.parse(readFileSync(join(version, 'nanocodex-resources/voice/runtime.json'), 'utf8'));
    if (receipt.engine === 'libwebrtc') {
      assert.equal(receipt.linkage, 'static', 'libWebRTC receipt linkage');
      assert.equal(receipt.helperBuildCommit, helper.stdout.trim(), 'receipt names the packaged helper build');
    } else {
      assert.equal(receipt.developmentOnly, false, 'GStreamer runtime must not be development-only');
      assert.equal(receipt.distribution, 'publicRelease', 'GStreamer runtime must be a sealed public release');
    }
    const recorder = join(version, 'nanocodex-resources/voice/bin/nanocodex-voice-recorder');
    let launched = 'helper --build-commit ' + helper.stdout.trim();
    if (process.platform === 'darwin') {
      // No arguments: the recorder rejects its usage before any microphone
      // permission check or capture, so this cannot open an audio device.
      const usage = run(recorder, [], { expect: 2, timeout: 10_000 });
      assert.match(usage.stderr, /^Usage: nanocodex-voice-recorder --max-seconds/m);
      launched += '; recorder usage exit 2';
    }
    pass(label + ': versions/' + key + ' has ' + programs.length + ' executable bin/ programs matching the archive (' +
      programs.map(name => name.split('/').pop() + ' ' + (statSync(join(version, name)).mode & 0o777).toString(8)).join(', ') + '); ' + launched);
  }
  run(runner, install);
  verifyPrograms('update --path --hand-binary --voice-archive');
  // Earlier updaters marked only the helper executable. Reselecting the same
  // pair must restore the verified 0600 programs' modes in the cached runtime.
  const unmarked = programs.filter(name => !/\/nanocodex-voice-host(\.exe)?$/.test(name));
  for (const name of unmarked) chmodSync(join(version, name), 0o600);
  const unmarkedNames = unmarked.map(name => name.split('/').pop()).join(', ');
  if (unmarked.length) {
    run(runner, install);
    verifyPrograms('reselecting a cached version whose ' + unmarkedNames + ' was 0600');
  }
  // Activation validates a cached version without any archive (also rollback
  // and hand recover). A staged pair is applied without --restart-hand, which
  // defers the Hand switch but still validates the version first.
  const pending = () => { try { return readFileSync(join(store, 'pending-update'), 'utf8').trim(); } catch { return null; } };
  if (unmarked.length && pending() === key) {
    for (const name of unmarked) chmodSync(join(version, name), 0o600);
    run(runner, ['update', '--apply']);
    assert.equal(pending(), key, 'apply without --restart-hand keeps the pair staged');
    verifyPrograms('update --apply validating staged ' + key + ' whose ' + unmarkedNames + ' was 0600');
  } else {
    summary.limitations.push('The pair was not staged (pending=' + pending() + '), so archive-free activation validation was not exercised.');
  }
  if (process.platform === 'darwin') {
    assert.equal(readFileSync(plist, 'utf8'), plistBefore, 'synthetic login owner unchanged');
  }
  summary.verdict = 'PASSED';
} catch (error) {
  trace.push('FAILED: ' + (error.stack ?? error));
  summary.error = String(error.message ?? error);
  throw error;
} finally {
  // Checked on failure too: no outcome may restart the user's Hand.
  const pidAfter = livePid();
  trace.push('live Hand after (read-only launchctl print): ' + pidAfter);
  summary.liveHand = { before: pidBefore, after: pidAfter };
  if (pidAfter !== pidBefore) {
    summary.verdict = 'FAILED';
    summary.error = (summary.error ? summary.error + '; ' : '') + 'live Hand PID changed';
    process.exitCode = 1;
  }
  save();
  if (summary.verdict === 'PASSED') rmSync(fixture, { recursive: true, force: true });
  console.log(summary.verdict + ': ' + join(output, 'transcript.log'));
}
