import type { MDXComponents } from 'mdx/types';
import { Cards, Card, Steps, Step, Callout } from '@/components/mdx';
import Link from 'next/link';

function createHeadingComponent(Tag: 'h2' | 'h3' | 'h4') {
  return function HeadingComponent({
    children,
    ...props
  }: React.ComponentPropsWithoutRef<typeof Tag>) {
    const id =
      typeof children === 'string'
        ? children
            .toLowerCase()
            .replace(/[^\w\s-]/g, '')
            .replace(/\s+/g, '-')
        : undefined;
    return (
      <Tag id={id} {...props}>
        {id ? (
          <a href={`#${id}`} className="no-underline">
            {children}
          </a>
        ) : (
          children
        )}
      </Tag>
    );
  };
}

const basePath = process.env.__NEXT_ROUTER_BASEPATH || '';

function MdxImage(props: React.ComponentPropsWithoutRef<'img'>) {
  const raw = typeof props.src === 'string' ? props.src : undefined;
  const src = raw?.startsWith('/') ? `${basePath}${raw}` : raw;
  // eslint-disable-next-line @next/next/no-img-element -- static export, next/image not applicable
  return <img {...props} src={src} alt={props.alt || ''} />;
}

function MdxLink({
  href,
  children,
  ...props
}: React.ComponentPropsWithoutRef<'a'>) {
  if (href?.startsWith('/') || href?.startsWith('#')) {
    return (
      <Link href={href} {...props}>
        {children}
      </Link>
    );
  }
  return (
    <a href={href} target="_blank" rel="noopener noreferrer" {...props}>
      {children}
    </a>
  );
}

export function getMDXComponents(
  components?: MDXComponents,
): MDXComponents {
  return {
    h2: createHeadingComponent('h2'),
    h3: createHeadingComponent('h3'),
    h4: createHeadingComponent('h4'),
    a: MdxLink,
    img: MdxImage,
    Cards,
    Card,
    Steps,
    Step,
    Callout,
    ...components,
  };
}
