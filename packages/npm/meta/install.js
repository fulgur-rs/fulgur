#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');

const PLATFORMS = {
  'linux-x64':      { os: 'linux',  cpu: 'x64',   pkg: '@fulgur-rs/cli-linux-x64',      bin: 'fulgur' },
  'linux-x64-musl': { os: 'linux',  cpu: 'x64',   pkg: '@fulgur-rs/cli-linux-x64-musl', bin: 'fulgur' },
  'linux-arm64':    { os: 'linux',  cpu: 'arm64', pkg: '@fulgur-rs/cli-linux-arm64',    bin: 'fulgur' },
  'darwin-arm64':   { os: 'darwin', cpu: 'arm64', pkg: '@fulgur-rs/cli-darwin-arm64',   bin: 'fulgur' },
  'darwin-x64':     { os: 'darwin', cpu: 'x64',   pkg: '@fulgur-rs/cli-darwin-x64',     bin: 'fulgur' },
  'win32-x64':      { os: 'win32',  cpu: 'x64',   pkg: '@fulgur-rs/cli-win32-x64',      bin: 'fulgur.exe' },
};

function isMusl() {
  try {
    const maps = fs.readFileSync('/proc/self/maps', 'utf8');
    return maps.includes('musl');
  } catch {
    return false;
  }
}

function detectPlatformKey() {
  const os = process.platform;
  const cpu = process.arch;

  if (os === 'linux' && cpu === 'x64') {
    return isMusl() ? 'linux-x64-musl' : 'linux-x64';
  }
  if (os === 'linux' && cpu === 'arm64') return 'linux-arm64';
  if (os === 'darwin' && cpu === 'arm64') return 'darwin-arm64';
  if (os === 'darwin' && cpu === 'x64') return 'darwin-x64';
  if (os === 'win32' && cpu === 'x64') return 'win32-x64';
  return null;
}

const platformKey = detectPlatformKey();
if (!platformKey) {
  process.stderr.write(
    `@fulgur-rs/cli: unsupported platform ${process.platform}/${process.arch}\n`
  );
  process.exit(1);
}

const platform = PLATFORMS[platformKey];
let pkgDir;
try {
  pkgDir = path.dirname(require.resolve(`${platform.pkg}/package.json`));
} catch {
  process.stderr.write(
    `@fulgur-rs/cli: platform package ${platform.pkg} not found.\n` +
    `This usually means it was not installed (e.g., --ignore-optional was used).\n`
  );
  process.exit(1);
}

const src = path.join(pkgDir, 'bin', platform.bin);
const destDir = path.join(__dirname, 'bin');
const dest = path.join(destDir, platform.bin);

if (!fs.existsSync(src)) {
  process.stderr.write(
    `@fulgur-rs/cli: binary not found at ${src}\n` +
    `The platform package ${platform.pkg} may be corrupted or incomplete.\n`
  );
  process.exit(1);
}

fs.mkdirSync(destDir, { recursive: true });
fs.copyFileSync(src, dest);
if (process.platform !== 'win32') {
  fs.chmodSync(dest, 0o755);
}
