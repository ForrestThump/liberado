import assert from "node:assert/strict";

/** Bytes for base64url "AQID" */
const CHALLENGE_BYTES = Uint8Array.of(0x01, 0x02, 0x03);
/** Bytes for base64url "AgME" */
const CRED_ID_BYTES = Uint8Array.of(0x02, 0x03, 0x04);
/** Bytes for base64url "BAUG" */
const USER_ID_BYTES = Uint8Array.of(0x04, 0x05, 0x06);

function bytesEqual(buf, expected) {
  const u8 = new Uint8Array(buf);
  if (u8.length !== expected.length) return false;
  for (let i = 0; i < u8.length; i++) {
    if (u8[i] !== expected[i]) return false;
  }
  return true;
}

function assertArrayBufferField(obj, field, expected) {
  assert.ok(obj[field] instanceof ArrayBuffer, `${field} must be ArrayBuffer`);
  assert.ok(bytesEqual(obj[field], expected), `${field} bytes`);
}

let lastCreateArgs = null;
let lastGetArgs = null;

const mockCred = {
  id: "cred-id",
  rawId: Uint8Array.of(9).buffer,
  type: "public-key",
  response: {
    clientDataJSON: Uint8Array.of(10).buffer,
    attestationObject: Uint8Array.of(11).buffer,
    getTransports() {
      return ["internal", "hybrid"];
    },
  },
  getClientExtensionResults() {
    return { credProps: { rk: true } };
  },
};

Object.defineProperty(globalThis, "navigator", {
  configurable: true,
  value: {
    credentials: {
      create: async (args) => {
        lastCreateArgs = args;
        return mockCred;
      },
      get: async (args) => {
        lastGetArgs = args;
        return mockCred;
      },
    },
  },
});

const { createPasskey, getPasskey } = await import("./webauthn.js");

const creationEnvelope = JSON.stringify({
  publicKey: {
    challenge: "AQID",
    rp: { name: "Liberado", id: "localhost" },
    user: { id: "BAUG", name: "u", displayName: "U" },
    pubKeyCredParams: [{ type: "public-key", alg: -7 }],
    excludeCredentials: [{ type: "public-key", id: "AgME" }],
    timeout: 300000,
  },
});

await createPasskey(creationEnvelope);
assert.ok(lastCreateArgs, "create must be called");
assert.ok(lastCreateArgs.publicKey, "publicKey option");
assert.equal(lastCreateArgs.publicKey.publicKey, undefined, "no nested publicKey");
assertArrayBufferField(lastCreateArgs.publicKey, "challenge", CHALLENGE_BYTES);
assertArrayBufferField(lastCreateArgs.publicKey.user, "id", USER_ID_BYTES);
assert.equal(lastCreateArgs.publicKey.excludeCredentials.length, 1);
assertArrayBufferField(lastCreateArgs.publicKey.excludeCredentials[0], "id", CRED_ID_BYTES);

const requestEnvelope = JSON.stringify({
  publicKey: {
    challenge: "AQID",
    rpId: "localhost",
    allowCredentials: [{ type: "public-key", id: "AgME" }],
    userVerification: "preferred",
  },
  mediation: "conditional",
});

await getPasskey(requestEnvelope);
assert.ok(lastGetArgs, "get must be called");
assert.equal(lastGetArgs.mediation, "conditional");
assert.ok(lastGetArgs.publicKey, "publicKey option");
assert.equal(lastGetArgs.publicKey.publicKey, undefined, "no nested publicKey");
assertArrayBufferField(lastGetArgs.publicKey, "challenge", CHALLENGE_BYTES);
assert.equal(lastGetArgs.publicKey.allowCredentials.length, 1);
assertArrayBufferField(lastGetArgs.publicKey.allowCredentials[0], "id", CRED_ID_BYTES);

const credJson = JSON.parse(await createPasskey(creationEnvelope));
assert.equal(credJson.response.clientDataJSON, "Cg");
assert.equal(credJson.response.attestationObject, "Cw");
assert.deepEqual(credJson.response.transports, ["internal", "hybrid"]);
assert.deepEqual(credJson.clientExtensionResults, { credProps: { rk: true } });

console.log("webauthn_node_test: ok");
