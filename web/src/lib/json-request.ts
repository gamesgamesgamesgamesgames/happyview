/** Whether a request sends JSON: any request with a JSON (string) body,
 * whatever its method, so a DELETE with a body is sent as JSON too. A POST,
 * PUT or PATCH without a body keeps the header, as it always has. */
export function sendsJson(options?: RequestInit): boolean {
  if (typeof options?.body === "string") return true;
  return (
    options?.method === "POST" ||
    options?.method === "PUT" ||
    options?.method === "PATCH"
  );
}
