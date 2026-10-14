// IOS-02: Command-] and Command-[ (next and previous task) from a cold
// relaunch, as UIKit key commands.
//
// SwiftUI's `keyboardShortcut` is reached only through the first
// responder's chain. After a relaunch iOS has no first responder until the
// user touches something, so on iPhone Command-] pressed right after a
// relaunch did nothing (IOS-02 rounds 1 and 7, on fresh Simulators). This
// view takes first-responder status only then: when it first appears in a
// window and when the app becomes active, and only if no responder holds it
// and no text input is editing. It yields as soon as a text input begins
// editing or the shell's Command-F asks for the search field, and never
// takes the keyboard back during that edit. Every other command (Command-F,
// Command-R, Command-,) stays a SwiftUI shortcut: from this view the key
// press goes up the responder chain to SwiftUI. It has no keyboard of its
// own and is not an accessibility element. Contract:
// docs/pending-tasks/ios-parity/ios-02/README.md.

import SwiftUI
import UIKit

extension Notification.Name {
    /// Posted before the shell moves the keyboard to a text input itself
    /// (Command-F): the key-command view gives up first responder at once.
    static let shellYieldKeyboard = Notification.Name("luna.shell.yield-keyboard")
}

struct ShellKeyCommand {
    let title: String
    let input: String
    let modifiers: UIKeyModifierFlags
    let action: () -> Void
}

struct ShellKeyCommands: UIViewRepresentable {
    let commands: [ShellKeyCommand]

    func makeUIView(context: Context) -> ShellKeyCommandView {
        ShellKeyCommandView()
    }

    func updateUIView(_ view: ShellKeyCommandView, context: Context) {
        view.commands = commands
    }
}

final class ShellKeyCommandView: UIView {
    var commands: [ShellKeyCommand] = []
    /// A text field or text view is editing: never claim meanwhile.
    private var textEditing = false
    nonisolated(unsafe) private var observers: [NSObjectProtocol] = []

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        isAccessibilityElement = false
        accessibilityElementsHidden = true
        backgroundColor = .clear
        observe(UIApplication.didBecomeActiveNotification) { $0.claimSoon() }
        observe(UITextField.textDidBeginEditingNotification) { $0.beganEditing() }
        observe(UITextView.textDidBeginEditingNotification) { $0.beganEditing() }
        observe(UITextField.textDidEndEditingNotification) { $0.textEditing = false }
        observe(UITextView.textDidEndEditingNotification) { $0.textEditing = false }
        observe(.shellYieldKeyboard) { $0.yield() }
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    deinit {
        for observer in observers {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    private func observe(_ name: Notification.Name, _ handle: @escaping (ShellKeyCommandView) -> Void) {
        observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) {
            [weak self] _ in
            MainActor.assumeIsolated { () -> Void in
                if let self { handle(self) }
            }
        })
    }

    override var canBecomeFirstResponder: Bool { true }

    override var keyCommands: [UIKeyCommand]? {
        commands.enumerated().map { index, command in
            let key = UIKeyCommand(title: command.title, action: #selector(runKeyCommand(_:)),
                                   input: command.input, modifierFlags: command.modifiers,
                                   propertyList: index)
            key.wantsPriorityOverSystemBehavior = true
            return key
        }
    }

    @objc private func runKeyCommand(_ sender: UIKeyCommand) {
        guard let index = sender.propertyList as? Int, commands.indices.contains(index) else { return }
        commands[index].action()
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        claimSoon()
    }

    private func beganEditing() {
        textEditing = true
        yield()
    }

    private func yield() {
        if isFirstResponder { resignFirstResponder() }
    }

    /// After the current event, so a responder that is about to take over
    /// does so first.
    private func claimSoon() {
        DispatchQueue.main.async { [weak self] in self?.claimIfUnclaimed() }
    }

    /// Becomes first responder only when no responder holds it and no text
    /// input is editing.
    private func claimIfUnclaimed() {
        guard window != nil, !isFirstResponder, !textEditing else { return }
        let current = UIResponder.shellCurrentFirstResponder()
        guard current == nil || current is UIWindow || current is UIApplication else { return }
        becomeFirstResponder()
    }
}

extension UIResponder {
    private static weak var shellFoundResponder: UIResponder?

    /// The current first responder, found by sending an action to nil.
    static func shellCurrentFirstResponder() -> UIResponder? {
        shellFoundResponder = nil
        UIApplication.shared.sendAction(#selector(shellCaptureFirstResponder(_:)), to: nil, from: nil, for: nil)
        return shellFoundResponder
    }

    @objc private func shellCaptureFirstResponder(_ sender: Any?) {
        UIResponder.shellFoundResponder = self
    }
}
