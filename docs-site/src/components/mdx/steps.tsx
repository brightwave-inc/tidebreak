'use client';

import { Children, type ReactNode } from 'react';

interface StepsProps {
  children: ReactNode;
}

export function Steps({ children }: StepsProps) {
  const items = Children.toArray(children);

  return (
    <div className="not-prose">
      {items.map((child, i) => (
        <div key={i} className="relative flex gap-5">
          <div className="flex flex-col items-center">
            <div className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-full border-2 border-border bg-background text-sm font-medium text-muted-foreground">
              {i + 1}
            </div>
            {i < items.length - 1 && (
              <div className="w-px flex-1 bg-border" />
            )}
          </div>
          <div className="min-w-0 flex-1 pb-12">
            {child}
          </div>
        </div>
      ))}
    </div>
  );
}

interface StepProps {
  children: ReactNode;
}

export function Step({ children }: StepProps) {
  return (
    <div className="[&>h2]:mt-0 [&>h2]:mb-2 [&>h2]:text-lg [&>h2]:font-semibold [&>h2]:text-foreground [&>p]:mt-0 [&>p]:mb-3 [&>p]:text-base [&>p]:leading-relaxed [&>p]:text-foreground/90 [&>ul]:mt-2 [&>ul]:mb-3 [&>ul]:space-y-1.5 [&>ul]:pl-5 [&>ul]:list-disc [&_li]:text-base [&_li]:leading-relaxed [&_li]:text-foreground/90">
      {children}
    </div>
  );
}
