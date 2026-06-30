// Zero-knowledge pastebin client.
//
// Encrypts/decrypts entirely in the browser with AES-256-GCM (WebCrypto). The
// random key is base64url-encoded into the URL fragment (after `#`), which
// browsers never send to the server. The server therefore only ever sees
// ciphertext — it has zero knowledge of the plaintext or the key.
//
// Link format:  <origin>/#<paste-id>.<key>
// Stored blob:  base64(iv) + "." + base64(ciphertext)

// ---- base64 helpers (chunked, so large pastes don't blow the call stack) ----
function bytesToB64(bytes) {
  let binary = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}
function b64ToBytes(b64) {
  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}
function b64UrlEncode(b64) {
  return b64.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
function b64UrlDecode(s) {
  s = s.replace(/-/g, "+").replace(/_/g, "/");
  while (s.length % 4) s += "=";
  return s;
}

// ---- crypto ----
async function encryptText(plaintext) {
  const key = await crypto.subtle.generateKey(
    { name: "AES-GCM", length: 256 },
    true,
    ["encrypt", "decrypt"],
  );
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const data = new TextEncoder().encode(plaintext);
  const ctBuf = await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, data);
  const rawKey = new Uint8Array(await crypto.subtle.exportKey("raw", key));
  const blob = bytesToB64(iv) + "." + bytesToB64(new Uint8Array(ctBuf));
  const keyStr = b64UrlEncode(bytesToB64(rawKey));
  return { blob, keyStr };
}
async function decryptBlob(blob, keyStr) {
  const dot = blob.indexOf(".");
  const iv = b64ToBytes(blob.slice(0, dot));
  const ct = b64ToBytes(blob.slice(dot + 1));
  const rawKey = b64ToBytes(b64UrlDecode(keyStr));
  const key = await crypto.subtle.importKey("raw", rawKey, { name: "AES-GCM" }, false, ["decrypt"]);
  const ptBuf = await crypto.subtle.decrypt({ name: "AES-GCM", iv }, key, ct);
  return new TextDecoder().decode(ptBuf);
}

// ---- API ----
async function createPaste(blob, ttlSeconds, oneShot) {
  const body = { content: blob, one_shot: oneShot };
  if (ttlSeconds) body.ttl_seconds = ttlSeconds;
  const res = await fetch("/api/pastes", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error("create failed: HTTP " + res.status);
  return await res.json(); // { id, ... }
}
async function fetchRaw(id) {
  const res = await fetch("/raw/" + encodeURIComponent(id));
  if (res.status === 404) return null; // not found, expired, or already burned
  if (!res.ok) throw new Error("fetch failed: HTTP " + res.status);
  return await res.text();
}

// ---- UI ----
const $ = (id) => document.getElementById(id);

async function onCreate() {
  const content = $("content").value;
  if (!content) return;
  const btn = $("createBtn");
  btn.disabled = true;
  btn.textContent = "Encrypting…";
  try {
    const { blob, keyStr } = await encryptText(content);
    const ttl = $("ttl").value ? Number($("ttl").value) : null;
    const created = await createPaste(blob, ttl, $("oneshot").checked);
    const link = `${location.origin}/#${created.id}.${keyStr}`;
    $("link").innerHTML = `<a href="${link}">${link}</a>`;
    $("result").classList.remove("hidden");
  } catch (err) {
    alert("Failed: " + err.message);
  } finally {
    btn.disabled = false;
    btn.textContent = "Encrypt & create";
  }
}

async function showView(id, keyStr) {
  $("create").classList.add("hidden");
  $("view").classList.remove("hidden");
  try {
    const blob = await fetchRaw(id);
    if (blob === null) {
      $("error").textContent = "Paste not found, expired, or already viewed.";
      return;
    }
    $("output").textContent = await decryptBlob(blob, keyStr);
  } catch (_err) {
    $("error").textContent = "Could not decrypt — wrong key or corrupted data.";
  }
}

function init() {
  const hash = location.hash.slice(1);
  const dot = hash.indexOf(".");
  if (dot > 0) {
    showView(hash.slice(0, dot), hash.slice(dot + 1));
  } else {
    $("createBtn").addEventListener("click", onCreate);
  }
}
init();
