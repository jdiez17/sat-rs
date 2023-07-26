use super::{State, TransactionStep};
use spacepackets::cfdp::{
    pdu::{
        metadata::{MetadataGenericParams, MetadataPdu},
        CommonPduConfig, FileDirectiveType, PduError,
    },
    PduType,
};

pub struct DestinationHandler {
    step: TransactionStep,
    state: State,
    pdu_conf: CommonPduConfig,
    transaction_params: TransactionParams,
}

struct TransactionParams {
    metadata_params: MetadataGenericParams,
    src_file_name: [u8; u8::MAX as usize],
    src_file_name_len: usize,
    dest_file_name: [u8; u8::MAX as usize],
    dest_file_name_len: usize,
}

impl Default for TransactionParams {
    fn default() -> Self {
        Self {
            metadata_params: Default::default(),
            src_file_name: [0; u8::MAX as usize],
            src_file_name_len: Default::default(),
            dest_file_name: [0; u8::MAX as usize],
            dest_file_name_len: Default::default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DestError {
    /// File directive expected, but none specified
    DirectiveExpected,
    CantProcessPacketType(FileDirectiveType),
    // Received new metadata PDU while being already being busy with a file transfer.
    RecvdMetadataButIsBusy,
    EmptySrcFileField,
    EmptyDestFileField,
    Pdu(PduError),
}

impl From<PduError> for DestError {
    fn from(value: PduError) -> Self {
        Self::Pdu(value)
    }
}

impl DestinationHandler {
    pub fn new() -> Self {
        Self {
            step: TransactionStep::Idle,
            state: State::Idle,
            pdu_conf: CommonPduConfig::new_with_defaults(),
            transaction_params: Default::default(),
        }
    }

    pub fn insert_packet(
        &mut self,
        pdu_type: PduType,
        pdu_directive: Option<FileDirectiveType>,
        raw_packet: &[u8],
    ) -> Result<(), DestError> {
        match pdu_type {
            PduType::FileDirective => {
                if pdu_directive.is_none() {
                    return Err(DestError::DirectiveExpected);
                }
                self.handle_file_directive(pdu_directive.unwrap(), raw_packet)
            }
            PduType::FileData => self.handle_file_data(raw_packet),
        }
    }

    pub fn handle_file_data(&mut self, raw_packet: &[u8]) -> Result<(), DestError> {
        Ok(())
    }

    pub fn handle_file_directive(
        &mut self,
        pdu_directive: FileDirectiveType,
        raw_packet: &[u8],
    ) -> Result<(), DestError> {
        match pdu_directive {
            FileDirectiveType::EofPdu => todo!(),
            FileDirectiveType::FinishedPdu => todo!(),
            FileDirectiveType::AckPdu => todo!(),
            FileDirectiveType::MetadataPdu => self.handle_metadata_pdu(raw_packet),
            FileDirectiveType::NakPdu => todo!(),
            FileDirectiveType::PromptPdu => todo!(),
            FileDirectiveType::KeepAlivePdu => todo!(),
        };
        Ok(())
    }

    pub fn state_machine(&mut self) {
        match self.state {
            State::Idle => todo!(),
            State::BusyClass1Nacked => self.fsm_nacked(),
            State::BusyClass2Acked => todo!(),
        }
    }

    pub fn handle_metadata_pdu(&mut self, raw_packet: &[u8]) -> Result<(), DestError> {
        if self.state != State::Idle {
            return Err(DestError::RecvdMetadataButIsBusy);
        }
        let metadata_pdu = MetadataPdu::from_bytes(raw_packet)?;
        self.transaction_params.metadata_params = *metadata_pdu.metadata_params();
        let src_name = metadata_pdu.src_file_name();
        if src_name.is_empty() {
            return Err(DestError::EmptySrcFileField);
        }
        self.transaction_params.src_file_name[..src_name.len_value()]
            .copy_from_slice(src_name.value().unwrap());
        self.transaction_params.src_file_name_len = src_name.len_value();
        let dest_name = metadata_pdu.dest_file_name();
        if dest_name.is_empty() {
            return Err(DestError::EmptyDestFileField);
        }
        self.transaction_params.dest_file_name[..dest_name.len_value()]
            .copy_from_slice(dest_name.value().unwrap());
        self.transaction_params.dest_file_name_len = dest_name.len_value();
        Ok(())
    }

    pub fn handle_eof_pdu(&mut self, raw_packet: &[u8]) -> Result<(), DestError> {
        Ok(())
    }

    fn fsm_nacked(&self) {
        match self.step {
            TransactionStep::Idle => {
                // TODO: Should not happen. Determine what to do later
            }
            TransactionStep::TransactionStart => {}
            TransactionStep::ReceivingFileDataPdus => todo!(),
            TransactionStep::SendingAckPdu => todo!(),
            TransactionStep::TransferCompletion => todo!(),
            TransactionStep::SendingFinishedPdu => todo!(),
        }
    }

    /// Get the step, which denotes the exact step of a pending CFDP transaction when applicable.
    pub fn step(&self) -> TransactionStep {
        self.step
    }

    /// Get the step, which denotes whether the CFDP handler is active, and which CFDP class
    /// is used if it is active.
    pub fn state(&self) -> State {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic() {
        let dest_handler = DestinationHandler::new();
        assert_eq!(dest_handler.state(), State::Idle);
        assert_eq!(dest_handler.step(), TransactionStep::Idle);
    }
}
