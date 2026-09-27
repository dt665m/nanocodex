import SwiftUI
import InboxCore

/// Read-only account CRM. Each search owns its task, so an older response cannot
/// replace a newer query; navigating away cancels outstanding requests.
struct CRMView: View {
    @ObservedObject var model: InboxModel
    @State private var query = ""
    @State private var kind = "person"
    @State private var records: [JSON] = []
    @State private var cursor = ""
    @State private var loading = false
    @State private var error: String?
    @State private var revision = 0
    private var searchKey: String { "\(kind):\(query):\(revision)" }

    var body: some View {
        List {
            Section {
                Text("CRM").font(.largeTitle.bold())
                HStack(spacing: 8) {
                    filterButton("People", value: "person")
                    filterButton("Companies", value: "company")
                }
                TextField("Search CRM", text: $query)
                    .textInputAutocapitalization(.never)
                    .accessibilityIdentifier("crm-search")
            }
            ForEach(records, id: \.crmID) { record in
                NavigationLink {
                    CRMProfileView(model: model, recordID: record.crmID)
                } label: {
                    CRMRecordLabel(record: record)
                }.accessibilityIdentifier("crm-record-\(record.crmID)")
            }
            if loading { ProgressView().frame(maxWidth: .infinity) }
            if let error {
                Section {
                    Text(error).foregroundStyle(.secondary)
                    Button("Retry") { revision += 1 }
                }
            } else if !loading && records.isEmpty {
                ContentUnavailableView(query.isEmpty ? "No \(kind == "person" ? "people" : "companies") yet" : "No matches",
                                       systemImage: "person.2", description: Text("Saved CRM records appear here."))
            }
            if !cursor.isEmpty && !loading {
                Button("Load more") { Task { await load(more: true) } }
            }
        }
        .listStyle(.insetGrouped)
        .task(id: searchKey) {
            records = []; cursor = ""; error = nil
            await load(more: false)
        }
        .refreshable { revision += 1 }
    }

    private func filterButton(_ title: String, value: String) -> some View {
        Button { kind = value } label: {
            Text(title).font(.subheadline.weight(.medium))
                .frame(maxWidth: .infinity).padding(.vertical, 8)
                .background(kind == value ? Color.accentColor.opacity(0.15) : Color.clear,
                            in: RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.borderless)
        .accessibilityAddTraits(kind == value ? .isSelected : [])
        .accessibilityIdentifier("crm-filter-\(value)")
    }

    @MainActor private func load(more: Bool) async {
        let key = searchKey
        loading = true; error = nil
        defer { if key == searchKey { loading = false } }
        do {
            if !more { try await Task.sleep(for: .milliseconds(200)) }
            let result = try await model.crmRead(query: ["q": query, "kind": kind, "cursor": more ? cursor : ""])
            try Task.checkCancellation()
            guard key == searchKey else { return }
            if case .array(let rows) = result["records"] {
                records = more ? records + rows.filter { row in !records.contains { $0.crmID == row.crmID } } : rows
                cursor = result["next_cursor"].string
            } else { throw APIError.invalidResponse }
        } catch is CancellationError {} catch {
            if key == searchKey { self.error = error.localizedDescription }
        }
    }
}

private struct CRMRecordLabel: View {
    let record: JSON
    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: record["kind"].string == "company" ? "building.2" : "person.crop.circle")
                .font(.title2).foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 4) {
                Text(record["name"].string).font(.headline)
                if !record["title"].string.isEmpty { Text(record["title"].string).font(.subheadline).foregroundStyle(.secondary) }
            }
        }.padding(.vertical, 4)
    }
}

private struct CRMProfileView: View {
    @ObservedObject var model: InboxModel
    let recordID: String
    @State private var detail: JSON = .null
    @State private var pages: [String: [JSON]] = [:]
    @State private var cursors: [String: String] = [:]
    @State private var loading = false
    @State private var error: String?
    @State private var revision = 0

    var body: some View {
        List {
            if detail != .null {
                Section {
                    CRMRecordLabel(record: detail["record"])
                    ForEach(["email", "phone", "website"], id: \.self) { key in
                        let value = detail["record"][key].string
                        if !value.isEmpty { linkedText(value, kind: key) }
                    }
                    if !detail["record"]["company_id"].string.isEmpty {
                        NavigationLink("Company") {
                            CRMProfileView(model: model, recordID: detail["record"]["company_id"].string)
                        }
                    }
                    if !detail["research"]["summary"].string.isEmpty { Text(detail["research"]["summary"].string) }
                }
                Section("Social profiles & identities") {
                    if (pages["identities"] ?? []).isEmpty { Text("No identities saved").foregroundStyle(.secondary) }
                    ForEach(pages["identities"] ?? [], id: \.crmID) { item in
                        VStack(alignment: .leading) {
                            Text(item["kind"].string.capitalized).font(.caption).foregroundStyle(.secondary)
                            linkedText(item["value"].string, kind: item["kind"].string)
                        }
                    }
                    moreButton("identities")
                }
                Section("Education & facts") {
                    if (pages["facts"] ?? []).isEmpty { Text("No facts saved").foregroundStyle(.secondary) }
                    ForEach(pages["facts"] ?? [], id: \.crmID) { item in
                        VStack(alignment: .leading, spacing: 5) {
                            Text(item["predicate"].string.replacingOccurrences(of: ".", with: " · ").capitalized).font(.headline)
                            Text(item["value"].crmReadable)
                            provenance(item)
                        }
                    }
                    moreButton("facts")
                }
                Section("Work & relationships") {
                    if (pages["relationships"] ?? []).isEmpty { Text("No relationships saved").foregroundStyle(.secondary) }
                    ForEach(pages["relationships"] ?? [], id: \.crmID) { item in
                        let other = item["from_id"].string == recordID ? "to" : "from"
                        let targetID = item["\(other)_id"].string
                        NavigationLink {
                            CRMProfileView(model: model, recordID: targetID)
                        } label: {
                            VStack(alignment: .leading, spacing: 5) {
                                Text(item["\(other)_name"].string.isEmpty ? "View related profile" : item["\(other)_name"].string).font(.headline)
                                Text([item["type"].string.replacingOccurrences(of: "_", with: " "), item["role"].string].filter { !$0.isEmpty }.joined(separator: " · "))
                                if !item["description"].string.isEmpty { Text(item["description"].string) }
                                provenance(item)
                            }
                        }.accessibilityIdentifier("crm-related-\(targetID)")
                    }
                    moreButton("relationships")
                }
                Section("Notes") {
                    if (pages["notes"] ?? []).isEmpty { Text("No notes saved").foregroundStyle(.secondary) }
                    ForEach(pages["notes"] ?? [], id: \.crmID) { note in
                        VStack(alignment: .leading, spacing: 5) {
                            Text(note["body"].string)
                            Text(note["created_at"].crmDate).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    moreButton("notes")
                }
            }
            if loading { ProgressView() }
            if let error {
                Text(error).foregroundStyle(.secondary)
                Button("Retry") { revision += 1 }
            }
        }
        .textSelection(.enabled)
        .navigationTitle(detail["record"]["name"].string)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .task(id: revision) { await load() }
        .refreshable { await load() }
    }

    @ViewBuilder private func linkedText(_ value: String, kind: String) -> some View {
        if let url = Self.link(value, kind: kind) { Link(value, destination: url) }
        else { Text(value) }
    }
    private static func link(_ value: String, kind: String) -> URL? {
        let candidate: String
        if kind == "email" { candidate = "mailto:\(value)" }
        else if kind == "phone" { candidate = "tel:\(value)" }
        else if value.hasPrefix("https://") || value.hasPrefix("http://") { candidate = value }
        else if kind == "x" { candidate = "https://x.com/\(value.trimmingCharacters(in: CharacterSet(charactersIn: "@")))" }
        else if kind == "github" { candidate = "https://github.com/\(value)" }
        else if kind == "linkedin" { candidate = "https://www.linkedin.com/in/\(value)" }
        else if kind == "telegram" { candidate = "https://t.me/\(value.trimmingCharacters(in: CharacterSet(charactersIn: "@")))" }
        else if ["website", "domain"].contains(kind), !value.contains(":") { candidate = "https://\(value)" }
        else { return nil }
        guard let url = URL(string: candidate), ["https", "http", "mailto", "tel"].contains(url.scheme ?? "") else { return nil }
        return url
    }
    private func provenance(_ item: JSON) -> some View {
        Text([item["origin"].string, item["confidence"].string.isEmpty ? "" : "\(item["confidence"].string) confidence", item["effective_from"].string, item["effective_to"].string].filter { !$0.isEmpty }.joined(separator: " · "))
            .font(.caption).foregroundStyle(.secondary)
    }
    @ViewBuilder private func moreButton(_ section: String) -> some View {
        if !(cursors[section] ?? "").isEmpty {
            Button("Load more \(section)") { Task { await load(section: section) } }.disabled(loading)
        }
    }
    @MainActor private func load(section: String? = nil) async {
        guard !loading else { return }
        loading = true; error = nil
        defer { loading = false }
        do {
            let result: JSON
            if let section {
                if section == "notes" {
                    result = try await model.crmRead(id: recordID, query: ["notes_cursor": cursors[section] ?? ""])
                } else {
                    result = try await model.crmRead(id: recordID, section: section, query: ["cursor": cursors[section] ?? ""])
                }
                try Task.checkCancellation()
                pages[section, default: []] += result[section].array
                cursors[section] = result["next_cursor"].string
            } else {
                result = try await model.crmRead(id: recordID)
                try Task.checkCancellation()
                guard !result["record"].crmID.isEmpty else { throw APIError.invalidResponse }
                detail = result
                for key in ["notes", "identities", "facts", "relationships"] {
                    pages[key] = result[key].array
                    cursors[key] = result[key == "notes" ? "next_cursor" : "\(key)_next_cursor"].string
                }
            }
        } catch is CancellationError {} catch { self.error = error.localizedDescription }
    }
}

private extension JSON {
    var crmID: String { self["id"].string }
    var crmDate: String {
        if case .number(let milliseconds) = self {
            return Date(timeIntervalSince1970: milliseconds / 1000).formatted(date: .abbreviated, time: .omitted)
        }
        return string
    }
    var crmReadable: String {
        switch self {
        case .string(let value): return value
        case .object(let values): return values.keys.sorted().map { "\($0.replacingOccurrences(of: "_", with: " ").capitalized): \(values[$0]!.crmReadable)" }.joined(separator: "\n")
        case .array(let values): return values.map(\.crmReadable).joined(separator: "\n")
        default: return pretty
        }
    }
}
