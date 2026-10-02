'use strict';

const { spawn } = require('child_process');
const fs = require('fs');
const https = require('https');
const path = require('path');
const heartbeat = require('./heartbeat');

// Pinned release. The launcher never searches PATH for nab or nab-mcp.
const NAB_MCP_VERSION = '0.12.3';

const ASSET_BY_PLATFORM = {
  'darwin:arm64': 'nab-mcp-aarch64-apple-darwin',
  'darwin:x64': 'nab-mcp-x86_64-apple-darwin',
  'linux:arm64': 'nab-mcp-aarch64-unknown-linux-gnu',
  'linux:x64': 'nab-mcp-x86_64-unknown-linux-gnu',
  'win32:x64': 'nab-mcp-x86_64-pc-windows-msvc.exe',
};

function assetName() {
  const key = `${process.platform}:${process.arch}`;
  const name = ASSET_BY_PLATFORM[key];
  if (!name) {
    throw new Error(`nab-mcp ${NAB_MCP_VERSION} has no binary for ${key}`);
  }
  return name;
}

function assertReleaseUrl(urlString) {
  const parsed = new URL(urlString);
  if (parsed.protocol !== 'https:') {
    throw new Error('refusing non-https nab-mcp download');
  }
  const host = parsed.hostname;
  const allowed =
    host === 'github.com' ||
    host.endsWith('.github.com') ||
    host === 'githubusercontent.com' ||
    host.endsWith('.githubusercontent.com');
  if (!allowed) {
    throw new Error(`refusing nab-mcp download host ${host}`);
  }
}

function download(urlString, dest, redirectsLeft) {
  assertReleaseUrl(urlString);
  return new Promise((resolve, reject) => {
    const req = https.get(
      urlString,
      {
        headers: {
          'User-Agent': 'nab-claude-plugin',
          Accept: 'application/octet-stream',
        },
      },
      (res) => {
        const code = res.statusCode || 0;
        if (code >= 300 && code < 400 && res.headers.location) {
          res.resume();
          if (redirectsLeft <= 0) {
            reject(new Error('too many redirects downloading nab-mcp'));
            return;
          }
          const next = new URL(res.headers.location, urlString).toString();
          resolve(download(next, dest, redirectsLeft - 1));
          return;
        }
        if (code !== 200) {
          res.resume();
          reject(new Error(`nab-mcp download failed: HTTP ${code}`));
          return;
        }
        const partial = `${dest}.partial`;
        const out = fs.createWriteStream(partial);
        res.pipe(out);
        out.on('finish', () => {
          out.close(() => {
            try {
              fs.renameSync(partial, dest);
              fs.chmodSync(dest, 0o755);
              resolve();
            } catch (err) {
              reject(err);
            }
          });
        });
        out.on('error', (err) => {
          fs.rmSync(partial, { force: true });
          reject(err);
        });
      },
    );
    req.on('error', reject);
  });
}

async function binaryPath() {
  const pluginRoot = path.resolve(__dirname, '..');
  const asset = assetName();
  const cacheDir = path.join(pluginRoot, 'bin', 'cache', NAB_MCP_VERSION);
  fs.mkdirSync(cacheDir, { recursive: true });
  const binPath = path.join(cacheDir, asset);
  if (!fs.existsSync(binPath) || fs.statSync(binPath).size === 0) {
    const downloadPrefix = 'https://github.com/MikkoParkkola/nab/releases/download/v0.12.3/';
    if (downloadPrefix !== `https://github.com/MikkoParkkola/nab/releases/download/v${NAB_MCP_VERSION}/`) {
      throw new Error('nab-mcp version pin drifted');
    }
    await download(`${downloadPrefix}${asset}`, binPath, 5);
    fs.chmodSync(binPath, 0o755);
  }
  return binPath;
}

async function main() {
  // The downloaded 0.12.3 binary does not contain this client. The launcher
  // sends the daily POST, and a later build shares ~/.nab/telemetry.
  heartbeat.start({
    project: 'nab',
    version: NAB_MCP_VERSION,
    optOut: ['NAB_NO_TELEMETRY'],
    endpointEnv: 'NAB_TELEMETRY_ENDPOINT',
    stateParts: ['.nab', 'telemetry'],
  });
  const binPath = await binaryPath();
  const child = spawn(binPath, process.argv.slice(2), {
    stdio: 'inherit',
    shell: false,
  });
  child.on('error', (err) => {
    process.stderr.write(`${err.message}\n`);
    process.exit(127);
  });
  child.on('exit', (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code === null ? 1 : code);
  });
}

main().catch((err) => {
  process.stderr.write(`${err && err.message ? err.message : String(err)}\n`);
  process.exit(1);
});
