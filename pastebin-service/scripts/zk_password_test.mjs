#!/usr/bin/env node
// End-to-end zero-knowledge test for a *running* pastebin service.
//
// This Node script is a faithful stand-in for the browser client: it uses the
// same WebCrypto primitives (PBKDF2-SHA256 + AES-256-GCM) and the same envelope
// format as static/app.js. It:
//   1. encrypts a secret with a password and POSTs the ciphertext,
//   2. fetches the stored ciphertext and tries the WRONG password (must fail),
//   3. fetches again and tries the CORRECT password (must succeed),
//   4. prints what the server actually stored (ciphertext only — zero knowledge).
//
// Usage:
//   node scripts/zk_password_test.mjs
//   BASE_URL=http://127.0.0.1:8090 node scripts/zk_password_test.mjs
//
// Requires Node 18+ (global fetch) — no npm dependencies.

import { webcrypto } from "node:crypto";
const { subtle } = webcrypto;

const BASE_URL = process.env.BASE_URL || "http://127.0.0.1:8090";
const ITER = 100000;

const b64 = (bytes) => Buffer.from(bytes).toString("base64");
const unb64 = (s) => new Uint8Array(Buffer.from(s, "base64"));
const b64url = (s) => s.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const unb64url = (s) => {
  s = s.replace(/-/g, "+").replace(/_/g, "/");
  while (s.length % 4) s += "=";
  return s;
};

async function deriveKey(urlKey, password, salt, iter) {
  const pw = new TextEncoder().encode(password || "");
  const material = new Uint8Array(urlKey.length + pw.length);
  material.set(urlKey, 0);
  material.set(pw, urlKey.length);
  const base = await subtle.importKey("raw", material, "PBKDF2", false, ["deriveKey"]);
  return subtle.deriveKey(
    { name: "PBKDF2", salt, iterations: iter, hash: "SHA-256" },
    base,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
}

async function encrypt(text, password) {
  const urlKey = webcrypto.getRandomValues(new Uint8Array(32));
  const salt = webcrypto.getRandomValues(new Uint8Array(16));
  const iv = webcrypto.getRandomValues(new Uint8Array(12));
  const key = await deriveKey(urlKey, password, salt, ITER);
  const ct = new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv }, key, new TextEncoder().encode(text)));
  const envelope = JSON.stringify({ v: 2, iter: ITER, salt: b64(salt), iv: b64(iv), ct: b64(ct) });
  return { envelope, keyStr: b64url(b64(urlKey)) };
}

async function decrypt(env, keyStr, password) {
  const key = await deriveKey(unb64(unb64url(keyStr)), password, unb64(env.salt), env.iter || ITER);
  const pt = await subtle.decrypt({ name: "AES-GCM", iv: unb64(env.iv) }, key, unb64(env.ct));
  return new TextDecoder().decode(pt);
}

async function main() {
  const secret = "the eagle lands at midnight";
  const password = "correct horse battery staple";
  let failures = 0;

  console.log(`== zero-knowledge password test against ${BASE_URL} ==`);

  console.log("[1] encrypt with password + create paste");
  const { envelope, keyStr } = await encrypt(secret, password);
  const res = await fetch(`${BASE_URL}/api/pastes`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content: envelope, one_shot: false }),
  });
  if (!res.ok) throw new Error(`create failed: HTTP ${res.status}`);
  const { id } = await res.json();
  console.log(`    id=${id}`);
  console.log(`    link: ${BASE_URL}/#${id}.${keyStr}   (password shared separately)`);

  console.log("[2] fetch ciphertext, try WRONG password");
  const env1 = JSON.parse(await (await fetch(`${BASE_URL}/raw/${id}`)).text());
  try {
    await decrypt(env1, keyStr, "not-the-password");
    console.log("    FAIL: wrong password decrypted!");
    failures++;
  } catch {
    console.log("    PASS: wrong password rejected (AES-GCM authentication failed)");
  }

  console.log("[3] fetch ciphertext, try CORRECT password");
  const env2 = JSON.parse(await (await fetch(`${BASE_URL}/raw/${id}`)).text());
  const out = await decrypt(env2, keyStr, password);
  if (out === secret) {
    console.log(`    PASS: decrypted -> ${JSON.stringify(out)}`);
  } else {
    console.log("    FAIL: decrypted text mismatch");
    failures++;
  }

  console.log("[4] what the server actually stored (ciphertext only):");
  console.log(`    ${JSON.stringify(env2)}`);

  console.log(failures === 0 ? "== ALL PASS ==" : `== ${failures} FAILED ==`);
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((err) => {
  console.error("ERROR:", err.message);
  process.exit(1);
});
