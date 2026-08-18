import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Badge,
} from "tidebreak-desktop-ui";

export function WorkspaceList() {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Workspace</TableHead>
          <TableHead>Branch</TableHead>
          <TableHead>Status</TableHead>
          <TableHead style={{ textAlign: "right" }}>Changes</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        <TableRow>
          <TableCell>Fix flaky retry test</TableCell>
          <TableCell style={{ fontFamily: "var(--mono)" }}>tb/fix-retry-test</TableCell>
          <TableCell>
            <Badge variant="success" size="sm">
              PR open
            </Badge>
          </TableCell>
          <TableCell style={{ textAlign: "right" }}>+128 −41</TableCell>
        </TableRow>
        <TableRow>
          <TableCell>Migrate settings schema</TableCell>
          <TableCell style={{ fontFamily: "var(--mono)" }}>tb/settings-schema</TableCell>
          <TableCell>
            <Badge variant="info" size="sm">
              Running
            </Badge>
          </TableCell>
          <TableCell style={{ textAlign: "right" }}>+64 −12</TableCell>
        </TableRow>
        <TableRow>
          <TableCell>Terminal theme tokens</TableCell>
          <TableCell style={{ fontFamily: "var(--mono)" }}>tb/terminal-theme</TableCell>
          <TableCell>
            <Badge variant="warning" size="sm">
              Needs you
            </Badge>
          </TableCell>
          <TableCell style={{ textAlign: "right" }}>+9 −3</TableCell>
        </TableRow>
      </TableBody>
    </Table>
  );
}
