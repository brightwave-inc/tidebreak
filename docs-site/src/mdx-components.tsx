import type { MDXComponents } from 'mdx/types';
import defaultMdxComponents from 'fumadocs-ui/mdx';
import { Step, Steps } from 'fumadocs-ui/components/steps';

export function getMDXComponents(
  components?: MDXComponents,
): MDXComponents {
  return {
    ...defaultMdxComponents,
    Steps,
    Step,
    ...components,
  };
}
