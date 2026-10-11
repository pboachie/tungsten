// SPDX-License-Identifier: Apache-2.0

package main

import (
	"encoding/json"
	"fmt"
	"sort"
)

// The parts of the IR (specs/ir.schema.json, the document of `tungsten ir
// dump`) this emitter reads. Unknown members are ignored, so newer IR minor
// versions load.

type Ident struct {
	Wire  string   `json:"wire"`
	Words []string `json:"words"`
}

type Doc struct {
	Summary     string `json:"summary"`
	Description string `json:"description"`
}

type IR struct {
	IRVersion  string      `json:"ir_version"`
	Generator  Generator   `json:"generator"`
	API        APIInfo     `json:"api"`
	Namespaces []Namespace `json:"namespaces"`
	Types      struct {
		Types []NamedType `json:"types"`
	} `json:"types"`
	Auth  []AuthScheme `json:"auth"`
	Agent AgentModel   `json:"agent"`
}

type Generator struct {
	TungstenVersion string            `json:"tungsten_version"`
	Inputs          map[string]string `json:"inputs"`
}

type APIInfo struct {
	Name        Ident    `json:"name"`
	Title       string   `json:"title"`
	Description string   `json:"description"`
	Version     string   `json:"version"`
	Servers     []Server `json:"servers"`
}

type Server struct {
	URL string `json:"url"`
}

type Namespace struct {
	Name      Ident      `json:"name"`
	Title     string     `json:"title"`
	Resources []Resource `json:"resources"`
	Errors    struct {
		CodeField string `json:"code_field"`
	} `json:"errors"`
}

type Resource struct {
	Name       Ident       `json:"name"`
	PathPrefix string      `json:"path_prefix"`
	Doc        *Doc        `json:"doc"`
	Operations []Operation `json:"operations"`
	Children   []Resource  `json:"children"`
}

type NamedType struct {
	ID        string `json:"id"`
	Name      Ident  `json:"name"`
	Namespace string `json:"namespace"`
	Shape     Shape  `json:"shape"`
	Doc       *Doc   `json:"doc"`
	Recursive bool   `json:"recursive"`
}

// TypeRef is a named type id or an inline shape.
type TypeRef struct {
	Named  string
	Inline *Shape
}

func (t *TypeRef) UnmarshalJSON(data []byte) error {
	var raw struct {
		Kind  string          `json:"kind"`
		Value json.RawMessage `json:"value"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	switch raw.Kind {
	case "named":
		return json.Unmarshal(raw.Value, &t.Named)
	case "inline":
		t.Inline = &Shape{}
		return json.Unmarshal(raw.Value, t.Inline)
	}
	return fmt.Errorf("unknown type reference kind %q", raw.Kind)
}

type Primitive struct {
	Kind   string           `json:"kind"`
	Format *json.RawMessage `json:"format"`
}

// FormatName is the string format: a known name, the text of "other", or "".
func (p Primitive) FormatName() string {
	if p.Format == nil {
		return ""
	}
	var name string
	if json.Unmarshal(*p.Format, &name) == nil {
		return name
	}
	var other struct {
		Other string `json:"other"`
	}
	if json.Unmarshal(*p.Format, &other) == nil {
		return other.Other
	}
	return ""
}

type Constraints struct {
	Pattern          *string      `json:"pattern"`
	MinLength        *int64       `json:"min_length"`
	MaxLength        *int64       `json:"max_length"`
	Minimum          *json.Number `json:"minimum"`
	Maximum          *json.Number `json:"maximum"`
	ExclusiveMinimum *json.Number `json:"exclusive_minimum"`
	ExclusiveMaximum *json.Number `json:"exclusive_maximum"`
	MultipleOf       *json.Number `json:"multiple_of"`
}

func (c Constraints) Empty() bool {
	return c.Pattern == nil && c.MinLength == nil && c.MaxLength == nil && c.Minimum == nil && c.Maximum == nil &&
		c.ExclusiveMinimum == nil && c.ExclusiveMaximum == nil && c.MultipleOf == nil
}

type EnumValue struct {
	Value json.RawMessage `json:"value"`
	Name  Ident           `json:"name"`
	Doc   *Doc            `json:"doc"`
}

type Field struct {
	WireName    string          `json:"wire_name"`
	Name        Ident           `json:"name"`
	Ty          TypeRef         `json:"ty"`
	Presence    string          `json:"presence"`
	ReadOnly    bool            `json:"read_only"`
	WriteOnly   bool            `json:"write_only"`
	Deprecated  bool            `json:"deprecated"`
	Doc         *Doc            `json:"doc"`
	Constraints Constraints     `json:"constraints"`
	Sensitive   bool            `json:"sensitive"`
	Default     json.RawMessage `json:"default"`
}

func (f Field) Required() bool {
	return f.Presence == "required" || f.Presence == "required_nullable"
}

func (f Field) Nullable() bool {
	return f.Presence == "required_nullable" || f.Presence == "optional_nullable"
}

type Additional struct {
	Kind   string   `json:"kind"`
	Values *TypeRef `json:"values"`
}

type Variant struct {
	Name Ident   `json:"name"`
	Ty   TypeRef `json:"ty"`
	Tag  *string `json:"tag"`
}

type Discriminator struct {
	Property string `json:"property"`
}

// Shape is an IR shape; Kind selects the members that apply.
type Shape struct {
	Kind        string      `json:"kind"`
	Primitive   Primitive   `json:"primitive"`
	Constraints Constraints `json:"constraints"`
	// enum
	Base   Primitive   `json:"base"`
	Values []EnumValue `json:"values"`
	// const
	Value json.RawMessage `json:"value"`
	// array
	Items  *TypeRef `json:"items"`
	Min    *int64   `json:"min"`
	Max    *int64   `json:"max"`
	Unique bool     `json:"unique"`
	// map: Values is in MapValues
	MapValues *TypeRef `json:"-"`
	// record
	Fields     []Field    `json:"fields"`
	Additional Additional `json:"additional"`
	// union
	Variants      []Variant      `json:"variants"`
	Discriminator *Discriminator `json:"discriminator"`
	Strategy      string         `json:"strategy"`
	// intersection
	Members []TypeRef `json:"members"`
	// nullable
	Inner *TypeRef `json:"inner"`
}

func (s *Shape) UnmarshalJSON(data []byte) error {
	type plain Shape
	var p plain
	// `values` is a list of enum values for enums and a type reference for
	// maps: read it by kind.
	var head struct {
		Kind   string          `json:"kind"`
		Values json.RawMessage `json:"values"`
	}
	if err := json.Unmarshal(data, &head); err != nil {
		return err
	}
	if head.Kind == "map" {
		var rest map[string]json.RawMessage
		if err := json.Unmarshal(data, &rest); err != nil {
			return err
		}
		delete(rest, "values")
		stripped, err := json.Marshal(rest)
		if err != nil {
			return err
		}
		if err := json.Unmarshal(stripped, &p); err != nil {
			return err
		}
		*s = Shape(p)
		s.MapValues = &TypeRef{}
		return json.Unmarshal(head.Values, s.MapValues)
	}
	if err := json.Unmarshal(data, &p); err != nil {
		return err
	}
	*s = Shape(p)
	return nil
}

type Operation struct {
	ID         string          `json:"id"`
	Name       Ident           `json:"name"`
	Method     string          `json:"method"`
	Path       PathTemplate    `json:"path"`
	Doc        *Doc            `json:"doc"`
	Params     ParamSet        `json:"params"`
	Body       *Body           `json:"body"`
	Responses  []Response      `json:"responses"`
	Security   []SecurityReq   `json:"security"`
	Pagination *Pagination     `json:"pagination"`
	Stream     *StreamSpec     `json:"stream"`
	Deprecated bool            `json:"deprecated"`
	Status     OperationStatus `json:"status"`
	RPC        *RPCBinding     `json:"rpc"`
	Agent      AgentMeta       `json:"agent"`
}

type PathTemplate struct {
	Raw string `json:"raw"`
}

type ParamSet struct {
	Path   []Param `json:"path"`
	Query  []Param `json:"query"`
	Header []Param `json:"header"`
	Cookie []Param `json:"cookie"`
}

type Param struct {
	WireName   string          `json:"wire_name"`
	Name       Ident           `json:"name"`
	Ty         TypeRef         `json:"ty"`
	Required   bool            `json:"required"`
	Doc        *Doc            `json:"doc"`
	Style      string          `json:"style"`
	Explode    bool            `json:"explode"`
	Role       string          `json:"role"`
	Deprecated bool            `json:"deprecated"`
	Constant   json.RawMessage `json:"constant"`
}

type Body struct {
	Content  []BodyContent `json:"content"`
	Required bool          `json:"required"`
	Doc      *Doc          `json:"doc"`
}

type BodyContent struct {
	MediaType string  `json:"media_type"`
	Ty        TypeRef `json:"ty"`
	Encoding  string  `json:"encoding"`
}

type StatusMatch struct {
	Kind  string `json:"kind"`
	Value int    `json:"value"`
}

type Response struct {
	Status  StatusMatch   `json:"status"`
	Content []BodyContent `json:"content"`
	Kind    struct {
		Kind string `json:"kind"`
	} `json:"kind"`
}

type SecurityReq struct {
	AllOf []struct {
		Scheme string `json:"scheme"`
	} `json:"all_of"`
}

type Pagination struct {
	Style struct {
		Kind          string `json:"kind"`
		RequestParam  string `json:"request_param"`
		ResponseField string `json:"response_field"`
		OffsetParam   string `json:"offset_param"`
		LimitParam    string `json:"limit_param"`
		PageParam     string `json:"page_param"`
		SizeParam     string `json:"size_param"`
	} `json:"style"`
	ItemsField      string  `json:"items_field"`
	PageSizeParam   *string `json:"page_size_param"`
	HasMoreField    *string `json:"has_more_field"`
	CursorItemField *string `json:"cursor_item_field"`
}

type StreamSpec struct {
	Event       TypeRef `json:"event"`
	Done        *string `json:"done"`
	RequestFlag *string `json:"request_flag"`
}

type OperationStatus struct {
	Kind string `json:"kind"`
	Gate *struct {
		EnvVar         string `json:"env_var"`
		DisabledStatus int    `json:"disabled_status"`
	} `json:"gate"`
}

type RPCBinding struct {
	DiscriminatorField string                     `json:"discriminator_field"`
	DiscriminatorValue string                     `json:"discriminator_value"`
	ParamsField        string                     `json:"params_field"`
	Constants          map[string]json.RawMessage `json:"constants"`
}

type AgentMeta struct {
	Safety      string `json:"safety"`
	Idempotency struct {
		Policy          string `json:"policy"`
		Header          string `json:"header"`
		Format          string `json:"format"`
		PersistRequired bool   `json:"persist_required"`
		Note            string `json:"note"`
	} `json:"idempotency"`
	Preview struct {
		Mode      string `json:"mode"`
		Header    string `json:"header"`
		Value     string `json:"value"`
		Operation string `json:"operation"`
	} `json:"preview"`
	Confirmation *struct {
		SummaryFields []string `json:"summary_fields"`
		Message       string   `json:"message"`
	} `json:"confirmation"`
	Verify *struct {
		Operation      string          `json:"operation"`
		Args           json.RawMessage `json:"args"`
		Expect         json.RawMessage `json:"expect"`
		Terminal       json.RawMessage `json:"terminal"`
		PollIntervalMs *int64          `json:"poll_interval_ms"`
		PollBudgetMs   *int64          `json:"poll_budget_ms"`
	} `json:"verify"`
	Remediation             map[string]Remediation `json:"remediation"`
	RemediationNote         string                 `json:"remediation_note"`
	SensitiveResponseFields []string               `json:"sensitive_response_fields"`
	ShownOnce               bool                   `json:"shown_once"`
	CompactDoc              string                 `json:"compact_doc"`
	Hidden                  bool                   `json:"hidden"`
}

type Remediation struct {
	Category   string `json:"category"`
	Text       string `json:"text"`
	Retryable  string `json:"retryable"`
	NextAction string `json:"next_action"`
}

type AuthScheme struct {
	Kind      string          `json:"kind"`
	Name      string          `json:"name"`
	Location  string          `json:"location"`
	WireName  string          `json:"wire_name"`
	Prefix    string          `json:"prefix"`
	Flows     []OAuthFlow     `json:"flows"`
	Satisfies []string        `json:"satisfies"`
	Parts     []CompositePart `json:"parts"`
}

type OAuthFlow struct {
	Kind             string            `json:"kind"`
	TokenURL         string            `json:"token_url"`
	AuthorizationURL string            `json:"authorization_url"`
	RefreshURL       string            `json:"refresh_url"`
	Scopes           map[string]string `json:"scopes"`
}

func (f OAuthFlow) ScopeNames() []string {
	out := make([]string, 0, len(f.Scopes))
	for k := range f.Scopes {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

type CompositePart struct {
	Kind         string `json:"kind"`
	Name         string `json:"name"`
	EqualsCookie string `json:"equals_cookie"`
	FromConfig   string `json:"from_config"`
	MutationOnly bool   `json:"mutation_only"`
	Prefix       string `json:"prefix"`
}

type AgentModel struct {
	Macros            []Macro                `json:"macros"`
	Gates             map[string]string      `json:"gates"`
	ErrorCodes        map[string]Remediation `json:"error_codes"`
	AmbiguousStatuses []int                  `json:"ambiguous_statuses"`
	NonJSON           []struct {
		Status    int    `json:"status"`
		Media     string `json:"media"`
		Category  string `json:"category"`
		Retryable string `json:"retryable"`
		Text      string `json:"text"`
	} `json:"non_json"`
	Retries struct {
		ReadOnly        RetryPolicy `json:"read_only"`
		Mutating        RetryPolicy `json:"mutating"`
		HonorRetryAfter bool        `json:"honor_retry_after"`
	} `json:"retries"`
}

type RetryPolicy struct {
	Max    int    `json:"max"`
	BaseMs int64  `json:"base_ms"`
	MaxMs  int64  `json:"max_ms"`
	Jitter string `json:"jitter"`
}

type Macro struct {
	Name                    string          `json:"name"`
	Summary                 string          `json:"summary"`
	Safety                  string          `json:"safety"`
	Steps                   json.RawMessage `json:"steps"`
	Output                  json.RawMessage `json:"output"`
	Input                   json.RawMessage `json:"input"`
	Cluster                 string          `json:"cluster"`
	SensitiveResponseFields []string        `json:"sensitive_response_fields"`
	ShownOnce               bool            `json:"shown_once"`
}

// Operations walks every callable operation in IR order (namespace, then
// depth-first resources).
func (ir *IR) Operations() []*Operation {
	var out []*Operation
	var walk func(r *Resource)
	walk = func(r *Resource) {
		// Planned operations live outside the tree; never make one callable.
		for i := range r.Operations {
			if r.Operations[i].Status.Kind != "planned" {
				out = append(out, &r.Operations[i])
			}
		}
		for i := range r.Children {
			walk(&r.Children[i])
		}
	}
	for n := range ir.Namespaces {
		for r := range ir.Namespaces[n].Resources {
			walk(&ir.Namespaces[n].Resources[r])
		}
	}
	return out
}

// Type returns a named type.
func (ir *IR) Type(id string) *NamedType {
	i := sort.Search(len(ir.Types.Types), func(i int) bool { return ir.Types.Types[i].ID >= id })
	if i < len(ir.Types.Types) && ir.Types.Types[i].ID == id {
		return &ir.Types.Types[i]
	}
	for j := range ir.Types.Types {
		if ir.Types.Types[j].ID == id {
			return &ir.Types.Types[j]
		}
	}
	return nil
}

// Resolve follows named types (and named nullables) to a shape.
func (ir *IR) Resolve(t *TypeRef) *Shape {
	cur := t
	for i := 0; i <= len(ir.Types.Types); i++ {
		if cur.Inline != nil {
			return cur.Inline
		}
		nt := ir.Type(cur.Named)
		if nt == nil {
			return nil
		}
		if nt.Shape.Kind == "nullable" && nt.Shape.Inner != nil {
			cur = nt.Shape.Inner
			continue
		}
		return &nt.Shape
	}
	return nil
}
