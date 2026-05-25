package output

import (
	"encoding/json"
	"fmt"
	"io"
	"strings"
	"text/tabwriter"
	"time"

	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	sigsyaml "sigs.k8s.io/yaml"

	openshellv1 "github.com/dims/openshell-driver-substrate/cmd/kubectl-osh/pkg/proto/openshell/v1"
)

// Supported output formats.
const (
	FormatTable = ""     // default for list views
	FormatJSON  = "json"
	FormatYAML  = "yaml"
	FormatWide  = "wide" // table + extra columns
)

// ValidFormats returns the accepted values for -o.
func ValidFormats() []string {
	return []string{FormatJSON, FormatYAML, FormatWide}
}

// PrintSandbox writes a single Sandbox to w in the requested format.
func PrintSandbox(w io.Writer, s *openshellv1.Sandbox, format string) error {
	switch format {
	case FormatJSON:
		return writeJSON(w, s)
	case FormatYAML:
		return writeYAML(w, s)
	case FormatTable, FormatWide:
		return writeTable(w, []*openshellv1.Sandbox{s}, format == FormatWide)
	default:
		return fmt.Errorf("unsupported output format %q (use one of: %s)", format, strings.Join(ValidFormats(), ", "))
	}
}

// PrintSandboxList writes a list of Sandboxes to w in the requested format.
func PrintSandboxList(w io.Writer, list []*openshellv1.Sandbox, format string) error {
	switch format {
	case FormatJSON:
		// Wrap in a top-level object for jq-friendliness.
		out := struct {
			Sandboxes []json.RawMessage `json:"sandboxes"`
		}{Sandboxes: make([]json.RawMessage, 0, len(list))}
		for _, s := range list {
			b, err := protojson.Marshal(s)
			if err != nil {
				return err
			}
			out.Sandboxes = append(out.Sandboxes, b)
		}
		enc := json.NewEncoder(w)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	case FormatYAML:
		// One YAML doc per sandbox, separated by `---`.
		for i, s := range list {
			if i > 0 {
				fmt.Fprintln(w, "---")
			}
			if err := writeYAML(w, s); err != nil {
				return err
			}
		}
		return nil
	case FormatTable, FormatWide:
		return writeTable(w, list, format == FormatWide)
	default:
		return fmt.Errorf("unsupported output format %q (use one of: %s)", format, strings.Join(ValidFormats(), ", "))
	}
}

func writeJSON(w io.Writer, m proto.Message) error {
	b, err := protojson.MarshalOptions{Multiline: true, Indent: "  "}.Marshal(m)
	if err != nil {
		return err
	}
	_, err = w.Write(append(b, '\n'))
	return err
}

func writeYAML(w io.Writer, m proto.Message) error {
	// protojson -> sigsyaml gives us kubectl-style YAML with camelCase keys.
	jb, err := protojson.Marshal(m)
	if err != nil {
		return err
	}
	yb, err := sigsyaml.JSONToYAML(jb)
	if err != nil {
		return err
	}
	_, err = w.Write(yb)
	return err
}

func writeTable(w io.Writer, list []*openshellv1.Sandbox, wide bool) error {
	tw := tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
	if wide {
		fmt.Fprintln(tw, "NAME\tID\tPHASE\tIMAGE\tAGE")
	} else {
		fmt.Fprintln(tw, "NAME\tID\tPHASE\tAGE")
	}
	for _, s := range list {
		name := s.GetMetadata().GetName()
		id := s.GetMetadata().GetId()
		phase := s.GetPhase().String()
		// Strip the `SANDBOX_PHASE_` prefix for readability.
		phase = strings.TrimPrefix(phase, "SANDBOX_PHASE_")
		age := humanAge(s.GetMetadata().GetCreatedAtMs())
		if wide {
			image := s.GetSpec().GetTemplate().GetImage()
			fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\n", name, id, phase, image, age)
		} else {
			fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", name, id, phase, age)
		}
	}
	return tw.Flush()
}

// humanAge converts a created-at unix-milliseconds timestamp into a
// short relative duration string (kubectl-style: 2m, 1h, 3d).
func humanAge(createdAtMs int64) string {
	if createdAtMs == 0 {
		return "<unknown>"
	}
	d := time.Since(time.UnixMilli(createdAtMs))
	if d < time.Minute {
		return fmt.Sprintf("%ds", int(d.Seconds()))
	}
	if d < time.Hour {
		return fmt.Sprintf("%dm", int(d.Minutes()))
	}
	if d < 24*time.Hour {
		return fmt.Sprintf("%dh", int(d.Hours()))
	}
	return fmt.Sprintf("%dd", int(d.Hours()/24))
}
