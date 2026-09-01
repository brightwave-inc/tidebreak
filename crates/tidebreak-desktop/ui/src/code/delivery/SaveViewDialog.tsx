import { Button } from "@/components/ui/button";
import {
  type CodeDeliveryPrViewFilters,
  type CodeDeliveryRunViewFilters,
  type CodeDeliverySavedView,
  type CodeDeliverySurface,
  useCodeDeliveryStore,
} from "../CodeDeliveryStore";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { clonePrFilters, cloneRunFilters } from "./helpers";
import { useState } from "react";

export function SaveViewDialog({
  open,
  onOpenChange,
  surface,
  filters,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  surface: CodeDeliverySurface;
  filters: CodeDeliveryPrViewFilters | CodeDeliveryRunViewFilters;
  onSaved: (id: string) => void;
}) {
  const [name, setName] = useState("");
  const save = () => {
    const trimmed = name.trim();
    if (!trimmed) return;
    const id = `${surface}:${Date.now()}`;
    const createdAt = new Date().toISOString();
    const view: CodeDeliverySavedView =
      surface === "pull_requests"
        ? {
            id,
            kind: surface,
            name: trimmed,
            filters: clonePrFilters(filters as CodeDeliveryPrViewFilters),
            createdAt,
          }
        : {
            id,
            kind: surface,
            name: trimmed,
            filters: cloneRunFilters(filters as CodeDeliveryRunViewFilters),
            createdAt,
          };
    useCodeDeliveryStore.getState().upsertSavedView(view);
    onSaved(id);
    setName("");
    onOpenChange(false);
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Save this view</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-2">
          <Label htmlFor="delivery-view-name">Name</Label>
          <Input
            id="delivery-view-name"
            value={name}
            placeholder="Production failures"
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                save();
              }
            }}
          />
          <p className="text-xs text-muted-foreground">
            Saves the current repositories, search, and filters on this device.
          </p>
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            Cancel
          </Button>
          <Button type="button" disabled={!name.trim()} onClick={save}>
            Save view
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
