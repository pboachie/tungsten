// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

Map<String, Object?> _map(Object? v) =>
    v is Map<String, Object?> ? v : <String, Object?>{};

List<Object?> _list(Object? v) => v is List ? v : const [];

String _str(Object? v) => v == null ? '' : '$v';

/// One parameter: [name] is its key in the arguments object (the SDK's
/// argument name), [wire] its name on the wire. Parameters with the roles
/// idempotency_key, origin, auth and constant are not arguments.
final class ParamDescriptor {
  ParamDescriptor._(Map<String, Object?> p)
      : key = _str(p['key']),
        name = _str(p['name'] ?? p['key']),
        wire = _str(p['wire']),
        location = _str(p['in']),
        required = p['required'] == true,
        style = p['style'] is String ? p['style']! as String : 'simple',
        explode = p['explode'] == true,
        role = p['role'] is String ? p['role']! as String : 'plain',
        constant = p['constant'];

  /// The arguments layout key.
  final String key;

  /// The SDK's argument name.
  final String name;

  /// The name on the wire.
  final String wire;

  /// `path`, `query`, `header` or `cookie`.
  final String location;

  /// Whether it must be given.
  final bool required;

  /// The serialization style.
  final String style;

  /// Whether arrays and objects explode.
  final bool explode;

  /// `plain`, `dry_run`, `idempotency_key`, `origin`, `auth` or `constant`.
  final String role;

  /// The value of a constant parameter.
  final Object? constant;

  /// Whether the caller passes it among the arguments.
  bool get isArgument => role == 'plain' || role == 'dry_run';
}

/// The request body: with [arg] set the whole body is that argument;
/// otherwise a JSON object of the merged [fields] ((name, wire) pairs).
final class BodyDescriptor {
  const BodyDescriptor._({
    required this.mediaType,
    required this.encoding,
    required this.required,
    required this.fields,
    required this.arg,
  });

  /// The media type.
  final String mediaType;

  /// `json`, `form`, `multipart`, `bytes` or `text`.
  final String encoding;

  /// Whether a body must be sent.
  final bool required;

  /// The merged fields: (argument name, wire name).
  final List<(String, String)> fields;

  /// The argument that is the whole body.
  final String? arg;
}

/// A declared response: [status] an int, `"4XX"` or `"default"`.
final class ResponseDescriptor {
  const ResponseDescriptor._(this.status, this.kind, this.mediaType);

  /// The status.
  final Object? status;

  /// `success`, `error` or `ambiguous`.
  final String kind;

  /// The media type.
  final String? mediaType;
}

/// What the runtime needs to call one operation, from the SDK descriptor
/// document v1, with every argument named by its `name`.
final class OperationDescriptor {
  /// One entry of the document's `operations` with the document's `defs`.
  OperationDescriptor(Map<String, Object?> doc, Map<String, Object?> defs)
      : id = _str(doc['id']),
        method = _str(doc['method']).toUpperCase(),
        path = _str(doc['path']),
        params = [
          for (final p in _list(doc['params'])) ParamDescriptor._(_map(p)),
        ],
        responses = [
          for (final r in _list(doc['responses']))
            ResponseDescriptor._(
              _map(r)['status'],
              _str(_map(r)['kind']),
              _map(r)['media_type'] as String?,
            ),
        ],
        security = [
          for (final alt in _list(doc['security']))
            [for (final n in _list(alt)) '$n'],
        ],
        rpc = doc['rpc'] is Map ? _map(doc['rpc']) : null,
        errorCodeField = doc['error_code_field'] as String?,
        summary = _str(doc['summary']),
        _defs = defs,
        _doc = doc {
    for (final p in params) {
      keyNames[p.key] = p.name;
    }
    body = _bodyOf(doc['body']);
    final status = _map(doc['status']);
    gate = status['kind'] == 'gated'
        ? {
            'env_var': status['env_var'],
            'disabled_status': status['disabled_status'],
          }
        : null;
    pagination = _paginationOf(doc['pagination']);
    agent = _agentOf(_map(doc['agent']));
    request = doc['request'] is Map ? Schema(_map(doc['request']), defs) : null;
    response =
        doc['response'] is Map ? Schema(_map(doc['response']), defs) : null;
    pageItem =
        doc['page_item'] is Map ? Schema(_map(doc['page_item']), defs) : null;
    final s = doc['stream'];
    stream = s is Map
        ? _StreamSpec(
            Schema(_map(_map(s)['event']), defs),
            _map(s)['done'] as String?,
            _map(s)['flag'] as String?,
          )
        : null;
  }

  final Map<String, Object?> _doc;
  final Map<String, Object?> _defs;

  /// The operation id.
  final String id;

  /// The HTTP method, upper-case.
  final String method;

  /// The path template.
  final String path;

  /// Every parameter.
  final List<ParamDescriptor> params;

  /// The declared responses.
  final List<ResponseDescriptor> responses;

  /// An OR of AND-sets of scheme names.
  final List<List<String>> security;

  /// The RPC envelope of an rpc_unflatten operation.
  final Map<String, Object?>? rpc;

  /// Where the API puts its error code.
  final String? errorCodeField;

  /// The summary.
  final String summary;

  /// Argument names by arguments layout key.
  final Map<String, String> keyNames = {};

  /// The request body.
  late final BodyDescriptor? body;

  /// The gate of a gated operation.
  late final Map<String, Object?>? gate;

  /// The pagination, with argument names.
  late final Map<String, Object?>? pagination;

  /// The agent metadata, with defaults.
  late final Map<String, Object?> agent;

  /// The validator of the arguments.
  late final Schema? request;

  /// The validator of a success body.
  late final Schema? response;

  /// The validator of page items.
  late final Schema? pageItem;

  late final _StreamSpec? stream;

  /// Whether the operation has an event stream.
  bool get hasStream => stream != null;

  /// The parameters the caller supplies as arguments.
  List<ParamDescriptor> get argumentParams =>
      [for (final p in params) if (p.isArgument) p];

  /// The safety tier.
  String get safety => agent['safety']! as String;

  /// Whether the operation can change state (it is not read-only).
  bool get isMutation => safety != 'read_only';

  /// Whether its result holds values the API shows only once.
  bool get shownOnce => agent['shown_once'] == true;

  /// The response fields redacted wherever the runtime shows them.
  List<String> get sensitiveResponseFields =>
      [for (final f in _list(agent['sensitive_response_fields'])) '$f'];

  /// The descriptor document entry this was read from.
  Map<String, Object?> get document => _doc;

  /// The document's `defs`.
  Map<String, Object?> get defs => _defs;

  Map<String, Object?> get _idempotency => _map(agent['idempotency']);

  Map<String, Object?> get _preview => _map(agent['preview']);

  /// The argument name of an arguments layout key, or the key itself.
  String nameOfKey(String key) => keyNames[key] ?? key;

  /// The argument name of a parameter given by name or wire name.
  String argName(String key) {
    for (final p in params) {
      if (p.name == key) return p.name;
    }
    for (final p in params) {
      if (p.wire == key) return p.name;
    }
    return key;
  }

  BodyDescriptor? _bodyOf(Object? raw) {
    if (raw is! Map) return null;
    final b = _map(raw);
    final shape = _map(b['shape']);
    final fields = <(String, String)>[];
    String? arg;
    if (shape['kind'] == 'merged') {
      for (final f in _list(shape['fields'])) {
        final m = _map(f);
        final name = _str(m['name'] ?? m['key']);
        fields.add((name, _str(m['wire'])));
        keyNames[_str(m['key'])] = name;
      }
    } else {
      arg = _str(shape['name'] ?? shape['arg']);
      keyNames[_str(shape['arg'])] = arg;
    }
    return BodyDescriptor._(
      mediaType: _str(b['media_type']),
      encoding: b['encoding'] is String ? b['encoding']! as String : 'json',
      required: b['required'] == true,
      fields: fields,
      arg: arg,
    );
  }

  Map<String, Object?>? _paginationOf(Object? raw) {
    if (raw is! Map) return null;
    final out = <String, Object?>{..._map(raw)};
    for (final k in const [
      'request_param',
      'page_size_param',
      'offset_param',
      'limit_param',
      'page_param',
      'size_param',
    ]) {
      if (out[k] is String) out[k] = nameOfKey(out[k]! as String);
    }
    return out;
  }

  Map<String, Object?> _agentOf(Map<String, Object?> a) {
    final out = <String, Object?>{...a};
    out['safety'] = a['safety'] is String ? a['safety'] : 'read_only';
    out['idempotency'] = <String, Object?>{
      'policy': 'none',
      'persist_required': false,
      ..._map(a['idempotency']),
    };
    out['preview'] =
        a['preview'] is Map ? a['preview'] : <String, Object?>{'mode': 'local'};
    out['remediation'] = _map(a['remediation']);
    out['sensitive_response_fields'] = _list(a['sensitive_response_fields']);
    out['sensitive_request_fields'] = [
      for (final p in _list(a['sensitive_request_fields']))
        _sensitivePath('$p'),
    ];
    out['shown_once'] = a['shown_once'] == true;
    return out;
  }

  String _sensitivePath(String path) {
    final dot = path.indexOf('.');
    if (dot < 0) return nameOfKey(path);
    return '${nameOfKey(path.substring(0, dot))}.${path.substring(dot + 1)}';
  }
}

final class _StreamSpec {
  const _StreamSpec(this.event, this.done, this.flag);

  final Schema event;
  final String? done;
  final String? flag;
}

/// A macro in the canonical form, from the SDK descriptor document.
final class MacroDescriptor {
  MacroDescriptor._(Map<String, Object?> m)
      : name = _str(m['name']),
        summary = _str(m['summary']),
        safety = m['safety'] is String ? m['safety']! as String : 'read_only',
        steps = [for (final s in _list(m['steps'])) _map(s)],
        output = m['output'],
        input = _map(m['input']),
        sensitiveResponseFields = [
          for (final f in _list(m['sensitive_response_fields'])) '$f',
        ],
        shownOnce = m['shown_once'] == true,
        cluster = m['cluster'] as String?;

  /// The macro name.
  final String name;

  /// The summary.
  final String summary;

  /// The declared safety tier.
  final String safety;

  /// The steps.
  final List<Map<String, Object?>> steps;

  /// The output expression.
  final Object? output;

  /// The input: `extends`, `add`, `fields`.
  final Map<String, Object?> input;

  /// The response fields redacted wherever the runtime shows them.
  final List<String> sensitiveResponseFields;

  /// Whether its output holds values the API shows only once.
  final bool shownOnce;

  /// The cluster it belongs to.
  final String? cluster;

  /// The input fields: (key, name, required).
  List<(String, String, bool)> get inputFields => [
        for (final f in _list(input['fields']))
          (
            _str(_map(f)['key']),
            _str(_map(f)['name'] ?? _map(f)['key']),
            _map(f)['required'] == true,
          ),
      ];

  /// The operation whose arguments the input takes.
  String? get extendsOperation => input['extends'] as String?;

  /// The added fields as JSON Schemas.
  Map<String, Object?> get added => _map(input['add']);
}

/// The SDK descriptor document of a generated SDK (format
/// `tungsten-sdk-descriptors/1`), which the SDK passes as a string.
final class SdkDescriptors {
  SdkDescriptors._(Map<String, Object?> doc)
      : target = doc['target'] as String?,
        api = _map(doc['api']),
        defs = _map(doc['defs']) {
    _map(doc['operations']).forEach((id, o) {
      operations[id] = OperationDescriptor(_map(o), defs);
    });
    _map(doc['macros']).forEach((name, m) {
      macros[name] = MacroDescriptor._(_map(m));
    });
  }

  /// The format this runtime reads.
  static const String format = 'tungsten-sdk-descriptors/1';

  /// Parses a document; throws [FormatException] when it is not one.
  factory SdkDescriptors.parse(String text) {
    final Object? doc;
    try {
      doc = Json.parse(text);
    } on JsonParseException catch (e) {
      throw FormatException('not an SDK descriptor document: ${e.message}');
    }
    if (doc is! Map<String, Object?> || doc['format'] != format) {
      throw const FormatException(
        'not an SDK descriptor document (format tungsten-sdk-descriptors/1)',
      );
    }
    return SdkDescriptors._(doc);
  }

  /// The target the names are spelled for.
  final String? target;

  /// The API descriptor.
  final Map<String, Object?> api;

  /// The named types' schemas by type id.
  final Map<String, Object?> defs;

  /// Every operation by id.
  final Map<String, OperationDescriptor> operations = {};

  /// Every macro by name.
  final Map<String, MacroDescriptor> macros = {};

  final Map<String, Schema?> _schemas = {};

  /// An operation by id; throws [ArgumentError] when there is none.
  OperationDescriptor operation(String id) =>
      operations[id] ?? (throw ArgumentError('no operation $id'));

  /// A macro by name; throws [ArgumentError] when there is none.
  MacroDescriptor macro(String name) =>
      macros[name] ?? (throw ArgumentError('no macro $name'));

  /// A validator of the named type [id], or null when the document does not
  /// define it.
  Schema? schemaOf(String id) => _schemas.putIfAbsent(
        id,
        () => defs[id] is Map ? Schema(_map(defs[id]), defs) : null,
      );
}
