// The message the login page shows for the `error` the server sent it back
// with. The account it names comes from the URL, so it is shown only when it
// looks like a DID or a handle.

const DID = /^did:[a-z]+:[A-Za-z0-9._:%-]{1,2048}$/;
const HANDLE =
  /^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]{0,61}[a-z0-9]$/i;

const MESSAGES: Record<string, string> = {
  not_authorized: "Your account is not authorized to access this dashboard.",
};

function account(params: URLSearchParams): string | null {
  const did = params.get("did");
  if (!did || !DID.test(did)) return null;
  const handle = params.get("handle");
  return handle && HANDLE.test(handle) ? `@${handle} (${did})` : did;
}

export function loginErrorMessage(params: URLSearchParams): string | null {
  const error = params.get("error");
  if (!error) return null;
  if (error !== "not_authorized") return MESSAGES[error] ?? error;
  const who = account(params);
  let message = who
    ? `You signed in as ${who}, which isn't a user of this dashboard. Sign in with another account, or ask an admin to add this one.`
    : MESSAGES.not_authorized;
  if (params.get("hint") === "local") {
    message +=
      " Under happyview dev, the account in happyview.toml's [instance] superadmin can sign in.";
  }
  return message;
}
