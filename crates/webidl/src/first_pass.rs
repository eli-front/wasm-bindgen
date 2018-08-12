//! Because some WebIDL constructs are defined in multiple places
//! (keyword `partial` is used to add to an existing construct),
//! We need to first walk the webidl to collect all non-partial
//! constructs so that we have containers in which to put the
//! partial ones.
//!
//! Only `interface`s, `dictionary`s, `enum`s and `mixin`s can
//! be partial.

use std::collections::{BTreeMap, BTreeSet};

use weedle::argument::Argument;
use weedle::attribute::ExtendedAttribute;
use weedle::interface::StringifierOrStatic;
use weedle::mixin::MixinMembers;
use weedle;

use super::Result;
use util;
use util::camel_case_ident;

/// Collection of constructs that may use partial.
#[derive(Default)]
pub(crate) struct FirstPassRecord<'src> {
    pub(crate) interfaces: BTreeMap<&'src str, InterfaceData<'src>>,
    pub(crate) dictionaries: BTreeSet<&'src str>,
    pub(crate) enums: BTreeSet<&'src str>,
    /// The mixins, mapping their name to the webidl ast node for the mixin.
    pub(crate) mixins: BTreeMap<&'src str, Vec<&'src MixinMembers<'src>>>,
    pub(crate) typedefs: BTreeMap<&'src str, &'src weedle::types::Type<'src>>,
    pub(crate) namespaces: BTreeMap<&'src str, NamespaceData<'src>>,
}

/// We need to collect interface data during the first pass, to be used later.
#[derive(Default)]
pub(crate) struct InterfaceData<'src> {
    /// Whether only partial interfaces were encountered
    pub(crate) partial: bool,
    pub(crate) global: bool,
    pub(crate) operations: BTreeMap<OperationId<'src>, OperationData<'src>>,
    pub(crate) superclass: Option<&'src str>,
}

/// We need to collect namespace data during the first pass, to be used later.
pub(crate) struct NamespaceData<'src> {
    /// Whether only partial namespaces were encountered
    pub(crate) partial: bool,
    pub(crate) operations: BTreeMap<Option<&'src str>, OperationData<'src>>,
}

impl<'src> NamespaceData<'src> {
    /// Creates an empty node for a non-partial namespace.
    pub(crate) fn empty_non_partial() -> Self {
        Self {
            partial: false,
            operations: Default::default(),
        }
    }
    /// Creates an empty node for a partial namespace.
    pub(crate) fn empty_partial() -> Self {
        Self {
            partial: true,
            operations: Default::default(),
        }
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OperationId<'src> {
    Constructor,
    Operation(Option<&'src str>),
    IndexingGetter,
    IndexingSetter,
    IndexingDeleter,
}

#[derive(Default)]
pub(crate) struct OperationData<'src> {
    pub(crate) overloaded: bool,
    /// Map from argument names to whether they are the same for multiple overloads
    pub(crate) argument_names_same: BTreeMap<Vec<&'src str>, bool>,
}

/// Implemented on an AST node to populate the `FirstPassRecord` struct.
pub(crate) trait FirstPass<'src, Ctx> {
    /// Populate `record` with any constructs in `self`.
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, ctx: Ctx) -> Result<()>;
}

impl<'src> FirstPass<'src, ()> for [weedle::Definition<'src>] {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        for def in self {
            def.first_pass(record, ())?;
        }

        Ok(())
    }
}

impl<'src> FirstPass<'src, ()> for weedle::Definition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        use weedle::Definition::*;

        match self {
            Dictionary(dictionary) => dictionary.first_pass(record, ()),
            Enum(enum_) => enum_.first_pass(record, ()),
            Interface(interface) => interface.first_pass(record, ()),
            PartialInterface(interface) => interface.first_pass(record, ()),
            InterfaceMixin(mixin) => mixin.first_pass(record, ()),
            PartialInterfaceMixin(mixin) => mixin.first_pass(record, ()),
            Namespace(namespace) => namespace.first_pass(record, ()),
            PartialNamespace(namespace) => namespace.first_pass(record, ()),
            Typedef(typedef) => typedef.first_pass(record, ()),
            _ => {
                // Other definitions aren't currently used in the first pass
                Ok(())
            }
        }
    }
}

impl<'src> FirstPass<'src, ()> for weedle::DictionaryDefinition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        if !record.dictionaries.insert(self.identifier.0) {
            warn!("encountered multiple dictionary declarations of {}", self.identifier.0);
        }
        Ok(())
    }
}

impl<'src> FirstPass<'src, ()> for weedle::EnumDefinition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        if !record.enums.insert(self.identifier.0) {
            warn!("Encountered multiple enum declarations of {}", self.identifier.0);
        }

        Ok(())
    }
}

/// Helper function to add an operation to an interface.
fn first_pass_interface_operation<'src>(
    record: &mut FirstPassRecord<'src>,
    self_name: &'src str,
    id: OperationId<'src>,
    arguments: &[Argument<'src>],
)  -> Result<()> {
    let mut names = Vec::with_capacity(arguments.len());
    for argument in arguments {
        match argument {
            Argument::Single(arg) => names.push(arg.identifier.0),
            Argument::Variadic(_) => return Ok(()),
        }
    }
    record
        .interfaces
        .get_mut(self_name)
        .unwrap()
        .operations
        .entry(id)
        .and_modify(|operation_data| operation_data.overloaded = true)
        .or_default()
        .argument_names_same
        .entry(names)
        .and_modify(|same_argument_names| *same_argument_names = true)
        .or_insert(false);

    Ok(())
}

impl<'src> FirstPass<'src, ()> for weedle::InterfaceDefinition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        {
            let interface = record
                .interfaces
                .entry(self.identifier.0)
                .or_default();
            interface.partial = false;
            interface.superclass = self.inheritance.map(|s| s.identifier.0);
        }

        if util::is_chrome_only(&self.attributes) {
            return Ok(())
        }

        if let Some(attrs) = &self.attributes {
            for attr in &attrs.body.list {
                attr.first_pass(record, self.identifier.0)?;
            }
        }

        for member in &self.members.body {
            member.first_pass(record, self.identifier.0)?;
        }

        Ok(())
    }
}

impl<'src> FirstPass<'src, ()> for weedle::PartialInterfaceDefinition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        record
            .interfaces
            .entry(self.identifier.0)
            .or_insert_with(||
                InterfaceData {
                    partial: true,
                    operations: Default::default(),
                    global: false,
                    superclass: None,
                },
            );

        if util::is_chrome_only(&self.attributes) {
            return Ok(())
        }

        for member in &self.members.body {
            member.first_pass(record, self.identifier.0)?;
        }

        Ok(())
    }
}

impl<'src> FirstPass<'src, &'src str> for ExtendedAttribute<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, self_name: &'src str) -> Result<()> {
        match self {
            ExtendedAttribute::ArgList(list) if list.identifier.0 == "Constructor" => {
                first_pass_interface_operation(
                    record,
                    self_name,
                    OperationId::Constructor,
                    &list.args.body.list,
                )
            }
            ExtendedAttribute::NoArgs(name) if (name.0).0 == "Constructor" => {
                first_pass_interface_operation(
                    record,
                    self_name,
                    OperationId::Constructor,
                    &[],
                )
            }
            ExtendedAttribute::NamedArgList(list)
                if list.lhs_identifier.0 == "NamedConstructor" =>
            {
                first_pass_interface_operation(
                    record,
                    self_name,
                    OperationId::Constructor,
                    &list.args.body.list,
                )
            }
            ExtendedAttribute::Ident(id) if id.lhs_identifier.0 == "Global" => {
                record.interfaces.get_mut(self_name).unwrap().global = true;
                Ok(())
            }
            ExtendedAttribute::IdentList(id) if id.identifier.0 == "Global" => {
                record.interfaces.get_mut(self_name).unwrap().global = true;
                Ok(())
            }
            _ => Ok(())
        }
    }
}

impl<'src> FirstPass<'src, &'src str> for weedle::interface::InterfaceMember<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, self_name: &'src str) -> Result<()> {
        match self {
            weedle::interface::InterfaceMember::Operation(op) => {
                op.first_pass(record, self_name)
            }
            _ => Ok(()),
        }
    }
}

impl<'src> FirstPass<'src, &'src str> for weedle::interface::OperationInterfaceMember<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, self_name: &'src str) -> Result<()> {
        if !self.specials.is_empty() && self.specials.len() != 1 {
            warn!("Unsupported webidl operation {:?}", self);
            return Ok(())
        }
        if let Some(StringifierOrStatic::Stringifier(_)) = self.modifier {
            warn!("Unsupported webidl operation {:?}", self);
            return Ok(())
        }
        first_pass_interface_operation(
            record,
            self_name,
            match self.identifier.map(|s| s.0) {
                None => match self.specials.get(0) {
                    None => OperationId::Operation(None),
                    Some(weedle::interface::Special::Getter(weedle::term::Getter))
                        => OperationId::IndexingGetter,
                    Some(weedle::interface::Special::Setter(weedle::term::Setter))
                        => OperationId::IndexingSetter,
                    Some(weedle::interface::Special::Deleter(weedle::term::Deleter))
                        => OperationId::IndexingDeleter,
                    Some(weedle::interface::Special::LegacyCaller(weedle::term::LegacyCaller))
                        => return Ok(()),
                },
                Some(ref name) => OperationId::Operation(Some(name.clone())),
            },
            &self.args.body.list,
        )
    }
}

impl<'src> FirstPass<'src, ()> for weedle::InterfaceMixinDefinition<'src>{
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        record
            .mixins
            .entry(self.identifier.0)
            .or_insert_with(Default::default)
            .push(&self.members.body);
        Ok(())
    }
}

impl<'src> FirstPass<'src, ()> for weedle::PartialInterfaceMixinDefinition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        record
            .mixins
            .entry(self.identifier.0)
            .or_insert_with(Default::default)
            .push(&self.members.body);
        Ok(())
    }
}

impl<'src> FirstPass<'src, ()> for weedle::TypedefDefinition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        if util::is_chrome_only(&self.attributes) {
            return Ok(());
        }

        if record.typedefs.insert(self.identifier.0, &self.type_.type_).is_some() {
            warn!("Encountered multiple declarations of {}", self.identifier.0);
        }

        Ok(())
    }
}

impl<'src> FirstPass<'src, ()> for weedle::NamespaceDefinition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        record
            .namespaces
            .entry(self.identifier.0)
            .and_modify(|entry| entry.partial = false)
            .or_insert_with(NamespaceData::empty_non_partial);

        if util::is_chrome_only(&self.attributes) {
            return Ok(())
        }

        // We ignore all attributes.

        for member in &self.members.body {
            member.first_pass(record, self.identifier.0)?;
        }

        Ok(())
    }
}

impl<'src> FirstPass<'src, ()> for weedle::PartialNamespaceDefinition<'src> {
    fn first_pass(&'src self, record: &mut FirstPassRecord<'src>, (): ()) -> Result<()> {
        record
            .namespaces
            .entry(self.identifier.0)
            .or_insert_with(NamespaceData::empty_partial);

        if util::is_chrome_only(&self.attributes) {
            return Ok(())
        }

        for member in &self.members.body {
            member.first_pass(record, self.identifier.0)?;
        }

        Ok(())
    }
}

impl<'src> FirstPass<'src, &'src str> for weedle::namespace::NamespaceMember<'src> {
    fn first_pass(&'src self,
                  record: &mut FirstPassRecord<'src>,
                  namespace_name: &'src str) -> Result<()>
    {
        match self {
            weedle::namespace::NamespaceMember::Operation(op) => {
                op.first_pass(record, namespace_name)
            }
            _ => Ok(()),
        }
    }
}

impl<'src> FirstPass<'src, &'src str> for weedle::namespace::OperationNamespaceMember<'src> {
    fn first_pass(&'src self,
                  record: &mut FirstPassRecord<'src>,
                  namespace_name: &'src str) -> Result<()>
    {
        let identifier = self.identifier.map(|s| s.0);
        let arguments = &self.args.body.list;
        let mut names = Vec::with_capacity(arguments.len());
        for argument in arguments {
            match argument {
                Argument::Single(arg) => names.push(arg.identifier.0),
                Argument::Variadic(_) => return Ok(()),
            }
        }
        record
            .namespaces
            .get_mut(namespace_name)
            .unwrap() // call this after creating the namespace
            .operations
            .entry(identifier)
            .and_modify(|operation_data| operation_data.overloaded = true)
            .or_insert_with(Default::default)
            .argument_names_same
            .entry(names)
            .and_modify(|same_argument_names| *same_argument_names = true)
            .or_insert(false);
        Ok(())
    }
}

impl<'a> FirstPassRecord<'a> {
    pub fn all_superclasses<'me>(&'me self, interface: &str)
        -> impl Iterator<Item = String> + 'me
    {
        let mut set = BTreeSet::new();
        self.fill_superclasses(interface, &mut set);
        set.into_iter()
    }

    fn fill_superclasses(&self, interface: &str, set: &mut BTreeSet<String>) {
        let data = match self.interfaces.get(interface) {
            Some(data) => data,
            None => return,
        };
        let superclass = match &data.superclass {
            Some(class) => class,
            None => return,
        };
        if set.insert(camel_case_ident(superclass)) {
            self.fill_superclasses(superclass, set);
        }
    }
}
