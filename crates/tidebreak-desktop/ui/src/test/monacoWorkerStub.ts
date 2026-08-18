/** Constructor Vite's `?worker` imports would have produced. */
export default class MonacoWorkerStub {
  postMessage(_message?: unknown) {}
  terminate() {}
  addEventListener(
    _type: string,
    _listener: EventListenerOrEventListenerObject,
  ) {}
  removeEventListener(
    _type: string,
    _listener: EventListenerOrEventListenerObject,
  ) {}
  dispatchEvent(_event: Event) {
    return false;
  }
}
