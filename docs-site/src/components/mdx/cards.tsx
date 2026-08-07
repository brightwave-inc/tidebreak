import type { ReactNode, ComponentPropsWithoutRef } from 'react';
import Link from 'next/link';
import { cn } from '@/lib/utils';

interface CardsProps {
  children: ReactNode;
}

export function Cards({ children }: CardsProps) {
  return (
    <div className="not-prose grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
      {children}
    </div>
  );
}

interface CardProps extends ComponentPropsWithoutRef<'div'> {
  title: string;
  href?: string;
  children?: ReactNode;
}

export function Card({
  title,
  href,
  children,
  className,
  ...rest
}: CardProps) {
  const content = (
    <>
      <h3 className="text-[0.9375rem] font-semibold text-foreground">{title}</h3>
      {children}
    </>
  );

  const baseClass = cn(
    'rounded-lg border border-border bg-card p-5 transition-colors',
    className,
  );

  if (href) {
    return (
      <Link href={href} className={cn(baseClass, 'hover:bg-accent')}>
        {content}
      </Link>
    );
  }

  return (
    <div className={baseClass} {...rest}>
      {content}
    </div>
  );
}
