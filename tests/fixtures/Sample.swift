import SwiftUI

/// Main content view.
struct ContentView: View {
    @State private var count = 0

    var body: some View {
        VStack {
            Text("Count: \(count)")
            Button("Add") { count += 1 }
        }
    }

    func reset() {
        count = 0
        print("reset")
        log()
    }
}

protocol Store {
    func load() async throws -> [Item]
}

final class Model: ObservableObject {
    @Published var items: [Item] = []

    init(store: Store) {
        self.store = store
        self.items = []
        self.loaded = false
    }

    func refresh() async {
        let items = try? await store.load()
        self.items = items ?? []
        self.loaded = true
    }
}

extension Model {
    func clear() {
        items = []
        loaded = false
        print("cleared")
    }
}

enum Mode { case a, b }
