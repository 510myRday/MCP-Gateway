import { useEffect, useRef } from "react";

export function usePolling(callback: () => Promise<void> | void, intervalMs: number, enabled: boolean) {
  const callbackRef = useRef(callback);
  callbackRef.current = callback;

  useEffect(() => {
    if (!enabled) {
      return;
    }

    let stopped = false;
    const tick = async () => {
      if (stopped) return;
      await callbackRef.current();
    };

    void tick();
    const interval = setInterval(() => {
      void tick();
    }, intervalMs);

    return () => {
      stopped = true;
      clearInterval(interval);
    };
  }, [enabled, intervalMs]);
}