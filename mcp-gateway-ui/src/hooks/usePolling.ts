import { useEffect, useRef } from "react";

export type PollOutcome = "active" | "idle" | "error";

export interface UsePollingOptions {
  enabled: boolean;
  activeMs?: number;
  idleMs?: number;
  errorMs?: number;
  immediate?: boolean;
}

const DEFAULT_ACTIVE_MS = 3000;
const DEFAULT_IDLE_MS = 10000;
const DEFAULT_ERROR_MS = 5000;

export function usePolling(
  callback: () => Promise<PollOutcome | void> | PollOutcome | void,
  options: UsePollingOptions,
) {
  const {
    enabled,
    activeMs = DEFAULT_ACTIVE_MS,
    idleMs = DEFAULT_IDLE_MS,
    errorMs = DEFAULT_ERROR_MS,
    immediate = true,
  } = options;

  const callbackRef = useRef(callback);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const inFlightRef = useRef(false);
  const runTokenRef = useRef(0);
  callbackRef.current = callback;

  useEffect(() => {
    runTokenRef.current += 1;
    const runToken = runTokenRef.current;

    const clearTimer = () => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };

    if (!enabled) {
      clearTimer();
      return;
    }

    let stopped = false;
    const resolveDelay = (outcome: PollOutcome): number => {
      if (outcome === "idle") return idleMs;
      if (outcome === "error") return errorMs;
      return activeMs;
    };

    const schedule = (delayMs: number) => {
      if (stopped || runToken !== runTokenRef.current) {
        return;
      }
      clearTimer();
      timerRef.current = setTimeout(() => {
        void tick();
      }, Math.max(0, delayMs));
    };

    const tick = async () => {
      if (stopped || runToken !== runTokenRef.current) {
        return;
      }
      if (inFlightRef.current) {
        schedule(activeMs);
        return;
      }

      inFlightRef.current = true;
      let outcome: PollOutcome = "active";
      try {
        const result = await callbackRef.current();
        if (result === "active" || result === "idle" || result === "error") {
          outcome = result;
        }
      } catch {
        outcome = "error";
      } finally {
        inFlightRef.current = false;
      }

      if (stopped || runToken !== runTokenRef.current) {
        return;
      }
      schedule(resolveDelay(outcome));
    };

    schedule(immediate ? 0 : activeMs);

    return () => {
      stopped = true;
      clearTimer();
    };
  }, [activeMs, enabled, errorMs, idleMs, immediate]);
}
