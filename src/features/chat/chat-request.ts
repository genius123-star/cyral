export async function runChatRequest<T>(
  onStart: () => unknown | Promise<unknown>,
  send: () => T | Promise<T>,
  onError: () => unknown | Promise<unknown>,
): Promise<T> {
  await onStart();
  try {
    return await send();
  } catch (error) {
    await onError();
    throw error;
  }
}
