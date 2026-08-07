import type { ReactNode } from 'react';
import { Info, AlertTriangle, AlertCircle } from 'lucide-react';

interface CalloutProps {
  type?: 'info' | 'warn' | 'error';
  children: ReactNode;
}

const icons = {
  info: Info,
  warn: AlertTriangle,
  error: AlertCircle,
};

const styles = {
  info: 'border-blue-500/30 bg-blue-500/5 text-blue-900 dark:text-blue-200',
  warn: 'border-yellow-500/30 bg-yellow-500/5 text-yellow-900 dark:text-yellow-200',
  error: 'border-red-500/30 bg-red-500/5 text-red-900 dark:text-red-200',
};

export function Callout({ type = 'info', children }: CalloutProps) {
  const Icon = icons[type];

  return (
    <div className={`my-6 flex gap-3 rounded-lg border p-4 text-sm ${styles[type]}`}>
      <Icon className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="[&>p]:m-0">{children}</div>
    </div>
  );
}
