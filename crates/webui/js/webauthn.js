function base64UrlToBuffer(b64) {
  const pad = "=".repeat((4 - (b64.length % 4)) % 4);
  const base64 = (b64 + pad).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(base64);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out.buffer;
}

function bufferToBase64Url(buffer) {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function normalizeCreationOptions(obj) {
  const o = { ...obj };
  if (typeof o.challenge === "string") o.challenge = base64UrlToBuffer(o.challenge);
  if (o.user && typeof o.user.id === "string") {
    o.user = { ...o.user, id: base64UrlToBuffer(o.user.id) };
  }
  if (Array.isArray(o.excludeCredentials)) {
    o.excludeCredentials = o.excludeCredentials.map((c) => ({
      ...c,
      id: typeof c.id === "string" ? base64UrlToBuffer(c.id) : c.id,
    }));
  }
  return o;
}

function normalizeRequestOptions(obj) {
  const o = { ...obj };
  if (typeof o.challenge === "string") o.challenge = base64UrlToBuffer(o.challenge);
  if (Array.isArray(o.allowCredentials)) {
    o.allowCredentials = o.allowCredentials.map((c) => ({
      ...c,
      id: typeof c.id === "string" ? base64UrlToBuffer(c.id) : c.id,
    }));
  }
  return o;
}

function credentialToJson(cred) {
  const response = cred.response;
  const out = {
    id: cred.id,
    rawId: bufferToBase64Url(cred.rawId),
    type: cred.type,
    response: {
      clientDataJSON: bufferToBase64Url(response.clientDataJSON),
    },
  };
  if (response.attestationObject) {
    out.response.attestationObject = bufferToBase64Url(response.attestationObject);
  }
  if (response.authenticatorData) {
    out.response.authenticatorData = bufferToBase64Url(response.authenticatorData);
  }
  if (response.signature) {
    out.response.signature = bufferToBase64Url(response.signature);
  }
  if (response.userHandle) {
    out.response.userHandle = bufferToBase64Url(response.userHandle);
  }
  return JSON.stringify(out);
}

export async function createPasskey(optionsJson) {
  const options = normalizeCreationOptions(JSON.parse(optionsJson));
  const cred = await navigator.credentials.create({ publicKey: options });
  if (!cred) throw new Error("Passkey creation was cancelled");
  return credentialToJson(cred);
}

export async function getPasskey(optionsJson) {
  const options = normalizeRequestOptions(JSON.parse(optionsJson));
  const cred = await navigator.credentials.get({ publicKey: options });
  if (!cred) throw new Error("Passkey sign-in was cancelled");
  return credentialToJson(cred);
}
