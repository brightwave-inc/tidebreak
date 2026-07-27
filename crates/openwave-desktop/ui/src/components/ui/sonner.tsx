import { Toaster as Sonner } from "sonner";

import { useTheme } from "@/theme";

type ToasterProps = React.ComponentProps<typeof Sonner>;

/**
 * Toast host. Sonner needs to be told the resolved theme rather than reading a
 * class off the document, so it is driven from the app's own theme state.
 */
export function Toaster(props: ToasterProps) {
  const { resolved } = useTheme();

  return (
    <Sonner
      theme={resolved}
      className="toaster group"
      toastOptions={{
        classNames: {
          toast:
            "group toast group-[.toaster]:border-border group-[.toaster]:bg-background group-[.toaster]:text-foreground group-[.toaster]:shadow-lg",
          description: "group-[.toast]:text-muted-foreground",
          actionButton: "group-[.toast]:bg-primary group-[.toast]:text-primary-foreground",
          cancelButton: "group-[.toast]:bg-muted group-[.toast]:text-muted-foreground",
        },
      }}
      {...props}
    />
  );
}
