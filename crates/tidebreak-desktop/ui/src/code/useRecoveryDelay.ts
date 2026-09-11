import { useEffect, useState } from "react";

/** Brief disconnects settle before recovery adds status text. */
export const RECOVERY_NOTICE_DELAY_MS = 1500;

export function useRecoveryDelay(recovering: boolean): boolean {
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    setVisible(false);
    if (!recovering) return;
    const timer = setTimeout(() => setVisible(true), RECOVERY_NOTICE_DELAY_MS);
    return () => clearTimeout(timer);
  }, [recovering]);
  return recovering && visible;
}
