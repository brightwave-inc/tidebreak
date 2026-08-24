import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  Bell,
  Check,
  ChevronDown,
  CircleAlert,
  FileText,
  MoreHorizontal,
  Search,
  Settings2,
  Trash2,
} from "lucide-react";
import { useState } from "react";

import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { SegmentedControl } from "@/components/ui/segmented";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { WithTooltip } from "@/components/ui/tooltip";

function Section({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <section className="grid gap-4">
      <div className="max-w-xl">
        <h2 className="text-base font-semibold tracking-tight">{title}</h2>
        <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
          {description}
        </p>
      </div>
      {children}
    </section>
  );
}

function ControlsCatalog() {
  const [mode, setMode] = useState<"focus" | "review" | "ship">("review");
  const [notifications, setNotifications] = useState(true);
  const [includeLogs, setIncludeLogs] = useState<boolean | "indeterminate">(
    "indeterminate",
  );

  return (
    <div className="mx-auto grid w-full max-w-5xl gap-10 p-8">
      <Section
        title="Action hierarchy"
        description="Use one clear primary action. Keep secondary and destructive actions visually quieter until the user needs them."
      >
        <div className="flex flex-wrap items-center gap-2">
          <Button>Continue</Button>
          <Button variant="secondary">Save draft</Button>
          <Button variant="outline">Review changes</Button>
          <Button variant="ghost">Open details</Button>
          <Button variant="link">Read guidance</Button>
          <Button variant="destructive">
            <Trash2 aria-hidden="true" /> Remove
          </Button>
          <Button disabled>Unavailable</Button>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <WithTooltip label="Search conversations">
            <Button size="icon" variant="outline" aria-label="Search">
              <Search aria-hidden="true" />
            </Button>
          </WithTooltip>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline">
                View options <ChevronDown aria-hidden="true" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="w-52">
              <DropdownMenuCheckboxItem
                checked={notifications}
                onCheckedChange={(checked) =>
                  setNotifications(checked === true)
                }
              >
                Notify on completion
              </DropdownMenuCheckboxItem>
              <DropdownMenuItem>
                <Settings2 aria-hidden="true" /> Configure tools
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive">
                <Trash2 aria-hidden="true" /> Clear history
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button variant="ghost-destructive">Delete workspace</Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete this workspace?</AlertDialogTitle>
                <AlertDialogDescription>
                  Tidebreak removes the local workspace. The remote branch and
                  pull request stay available.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Keep workspace</AlertDialogCancel>
                <AlertDialogAction variant="destructive">
                  Delete workspace
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </div>
      </Section>

      <Section
        title="Forms and selection"
        description="Labels state the decision. Supporting text explains consequences without competing with the control."
      >
        <div className="grid gap-6 md:grid-cols-2">
          <Card>
            <CardHeader className="items-start">
              <div>
                <CardTitle>Session defaults</CardTitle>
                <CardDescription>
                  These values apply to the next conversation.
                </CardDescription>
              </div>
            </CardHeader>
            <CardContent className="gap-4">
              <div className="grid gap-1.5">
                <Label htmlFor="session-name">Session name</Label>
                <Input id="session-name" defaultValue="Review release flow" />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="provider">Provider</Label>
                <Select defaultValue="gateway">
                  <SelectTrigger id="provider">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="gateway">Model Gateway</SelectItem>
                    <SelectItem value="direct">Direct provider</SelectItem>
                    <SelectItem value="managed">Managed default</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="instructions">Instructions</Label>
                <Textarea
                  id="instructions"
                  rows={4}
                  defaultValue="Inspect the current flow, reproduce failures, and keep changes focused."
                />
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="items-start">
              <div>
                <CardTitle>Review behavior</CardTitle>
                <CardDescription>
                  Choose how much context the reviewer sees.
                </CardDescription>
              </div>
            </CardHeader>
            <CardContent className="gap-5">
              <SegmentedControl
                aria-label="Review mode"
                value={mode}
                onValueChange={setMode}
                options={[
                  { value: "focus", label: "Focus" },
                  { value: "review", label: "Review" },
                  { value: "ship", label: "Ship" },
                ]}
              />
              <RadioGroup defaultValue="changes" className="gap-3">
                <Label className="flex items-start gap-3 font-normal">
                  <RadioGroupItem value="changes" className="mt-0.5" />
                  <span>
                    <span className="block font-medium">Changed files</span>
                    <span className="mt-0.5 block text-sm text-muted-foreground">
                      Review the focused diff and nearby contracts.
                    </span>
                  </span>
                </Label>
                <Label className="flex items-start gap-3 font-normal">
                  <RadioGroupItem value="workspace" className="mt-0.5" />
                  <span>
                    <span className="block font-medium">Whole workspace</span>
                    <span className="mt-0.5 block text-sm text-muted-foreground">
                      Include unchanged files that shape the user flow.
                    </span>
                  </span>
                </Label>
              </RadioGroup>
              <div className="flex items-start justify-between gap-4 border-t pt-4">
                <div>
                  <Label htmlFor="notifications">
                    Completion notifications
                  </Label>
                  <p className="mt-1 text-sm text-muted-foreground">
                    Show a desktop notification when the run stops.
                  </p>
                </div>
                <Switch
                  id="notifications"
                  checked={notifications}
                  onCheckedChange={setNotifications}
                />
              </div>
              <Label className="flex items-center gap-2 font-normal">
                <Checkbox
                  checked={includeLogs}
                  onCheckedChange={setIncludeLogs}
                />
                Include relevant command output
              </Label>
            </CardContent>
            <CardFooter>Selected mode: {mode}</CardFooter>
          </Card>
        </div>
      </Section>
    </div>
  );
}

function FeedbackCatalog() {
  return (
    <div className="mx-auto grid w-full max-w-5xl gap-10 p-8">
      <Section
        title="Status and progress"
        description="Status uses both words and color. Progress stays tied to a clear task and next step."
      >
        <div className="flex flex-wrap gap-2">
          <Badge variant="success">Ready</Badge>
          <Badge variant="warning">Waiting</Badge>
          <Badge variant="critical">Failed</Badge>
          <Badge variant="info">Running</Badge>
          <Badge variant="merged">Merged</Badge>
          <Badge variant="outline">Not started</Badge>
        </div>
        <Card className="max-w-2xl">
          <CardHeader className="items-start justify-between">
            <div>
              <CardTitle>Building Storybook</CardTitle>
              <CardDescription>
                Compiling page stories and visual fixtures.
              </CardDescription>
            </div>
            <Badge variant="info">68%</Badge>
          </CardHeader>
          <Progress value={68} aria-label="Storybook build progress" />
          <CardFooter>18 of 26 story modules compiled</CardFooter>
        </Card>
      </Section>

      <Section
        title="Loading and empty states"
        description="Loading preserves the final layout. Empty states explain the value and offer one useful next action."
      >
        <div className="grid gap-6 md:grid-cols-2">
          <Card aria-label="Loading workspace list">
            <div className="flex items-center gap-3">
              <Skeleton className="size-9 rounded-lg" />
              <div className="grid flex-1 gap-2">
                <Skeleton className="h-4 w-40" />
                <Skeleton className="h-3 w-56 max-w-full" />
              </div>
            </div>
            <Skeleton className="h-24 w-full" />
            <div className="flex gap-2">
              <Skeleton className="h-7 w-24" />
              <Skeleton className="h-7 w-20" />
            </div>
          </Card>
          <Empty className="min-h-56 bg-background ring-1 ring-foreground/10">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <FileText aria-hidden="true" />
              </EmptyMedia>
              <EmptyTitle>No review notes</EmptyTitle>
              <EmptyDescription>
                Notes appear here when an agent finds a decision that needs your
                attention.
              </EmptyDescription>
            </EmptyHeader>
            <EmptyContent>
              <Button variant="outline">Review recent runs</Button>
            </EmptyContent>
          </Empty>
        </div>
      </Section>

      <Section
        title="Progressive disclosure"
        description="Keep the summary visible. Put details behind a labeled control so dense views stay calm."
      >
        <Accordion
          type="single"
          collapsible
          defaultValue="decision"
          className="max-w-2xl rounded-xl bg-background px-4 ring-1 ring-foreground/10"
        >
          <AccordionItem value="decision">
            <AccordionTrigger>Why does this need review?</AccordionTrigger>
            <AccordionContent className="text-muted-foreground">
              The change alters how Tidebreak resumes interrupted sessions. The
              reviewer needs to confirm that queued prompts remain in order.
            </AccordionContent>
          </AccordionItem>
          <AccordionItem value="evidence">
            <AccordionTrigger>Evidence and checks</AccordionTrigger>
            <AccordionContent className="text-muted-foreground">
              The focused reducer tests pass. The desktop integration lane has
              not finished yet.
            </AccordionContent>
          </AccordionItem>
        </Accordion>
      </Section>
    </div>
  );
}

const runRows = [
  {
    name: "Conversation states",
    owner: "Design review",
    status: "Ready",
    tone: "success" as const,
  },
  {
    name: "Document viewers",
    owner: "Visual review",
    status: "Running",
    tone: "info" as const,
  },
  {
    name: "Settings pages",
    owner: "Accessibility",
    status: "Needs input",
    tone: "warning" as const,
  },
  {
    name: "Code workspace",
    owner: "Interaction review",
    status: "Failed",
    tone: "critical" as const,
  },
];

function DenseDataCatalog() {
  return (
    <div className="mx-auto grid w-full max-w-5xl gap-8 p-8">
      <Section
        title="Dense information"
        description="Tabs separate modes. Rows align repeated facts so the user can scan status before opening details."
      >
        <Tabs defaultValue="active">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <TabsList aria-label="Run state">
              <TabsTrigger value="active">Active</TabsTrigger>
              <TabsTrigger value="attention">
                Needs attention <Badge variant="critical">1</Badge>
              </TabsTrigger>
              <TabsTrigger value="complete">Complete</TabsTrigger>
            </TabsList>
            <Button size="sm" variant="outline">
              <Bell aria-hidden="true" /> Notification settings
            </Button>
          </div>
          <TabsContent value="active">
            <div className="overflow-hidden rounded-xl bg-background ring-1 ring-foreground/10">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Area</TableHead>
                    <TableHead>Review lens</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead className="w-12">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {runRows.map((row) => (
                    <TableRow key={row.name}>
                      <TableCell className="font-medium">{row.name}</TableCell>
                      <TableCell className="text-muted-foreground">
                        {row.owner}
                      </TableCell>
                      <TableCell>
                        <Badge variant={row.tone}>{row.status}</Badge>
                      </TableCell>
                      <TableCell>
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          aria-label={`Open actions for ${row.name}`}
                        >
                          <MoreHorizontal aria-hidden="true" />
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          </TabsContent>
          <TabsContent value="attention">
            <Card className="max-w-2xl border-critical-border bg-critical-background">
              <CardHeader className="items-start">
                <CircleAlert
                  className="mt-0.5 size-4 text-critical"
                  aria-hidden="true"
                />
                <div>
                  <CardTitle>Code workspace needs attention</CardTitle>
                  <CardDescription className="text-critical-foreground-muted">
                    The compact browser toolbar hides the active viewport label.
                  </CardDescription>
                </div>
              </CardHeader>
              <CardFooter className="flex-row gap-2">
                <Button size="sm">Open story</Button>
                <Button size="sm" variant="outline">
                  View screenshot
                </Button>
              </CardFooter>
            </Card>
          </TabsContent>
          <TabsContent value="complete">
            <Empty className="min-h-48 bg-background ring-1 ring-foreground/10">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <Check aria-hidden="true" />
                </EmptyMedia>
                <EmptyTitle>No completed reviews yet</EmptyTitle>
                <EmptyDescription>
                  Completed areas move here after their P1 and P2 findings are
                  resolved.
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          </TabsContent>
        </Tabs>
      </Section>
    </div>
  );
}

const meta = {
  title: "Foundations/Primitives",
  component: ControlsCatalog,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof ControlsCatalog>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Controls: Story = {};

export const FeedbackAndDisclosure: Story = {
  render: () => <FeedbackCatalog />,
};

export const DenseData: Story = {
  render: () => <DenseDataCatalog />,
};
